#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
FastAPI backend serving multi-level (pressure-level) wind data parsed from an
ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    ./run.sh

Which file gets served is resolved from `data/catalog.json`, not hardcoded --
run `./.venv/bin/python catalog_data.py` to generate it. See README.md.

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.
"""

import json
import math
import os
import sys

import numpy as np
import xarray as xr
from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware

from wind_data import describe_wind_source

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
# There is deliberately no hardcoded data filename here. Copernicus downloads
# arrive named after an opaque request hash, so the file to serve is resolved
# at startup -- see resolve_source(). Everything below is an override knob.
HERE = os.path.dirname(os.path.abspath(__file__))
DATA_DIR = os.path.join(HERE, "data")

DEFAULT_SPATIAL_STRIDE = 2  # downsample every Nth grid point in lat/lon

CATALOG_COMMAND = "./.venv/bin/python catalog_data.py"


def _log(*args):
    """Print to stderr, flushed.

    Not cosmetic: plain print() goes to a *block-buffered* stdout when the
    process isn't attached to a tty, so under `run-all.sh` (which redirects to
    a log file) the startup banner would sit in the buffer indefinitely --
    invisible exactly when it matters most. uvicorn logs to stderr too, so this
    also keeps our lines interleaved with its own in the right order.
    """
    print(*args, file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# ISA (International Standard Atmosphere) pressure -> geometric altitude.
# Piecewise: troposphere (0-11km) + lower stratosphere isothermal layer
# (11-20km). This is an approximation -- fine for a simulator, not for
# real navigation. Balloon altitudes of interest (15-25km) sit mostly in
# the isothermal layer; error grows a bit above ~20km where the real
# atmosphere's lapse rate changes again, but stays close enough for this
# use case.
#
# sim-server/src/atmosphere.rs is a verbatim port of this and names this
# function as its source of truth -- keep them in step.
# ---------------------------------------------------------------------------
P0 = 1013.25   # hPa, sea-level standard pressure
T0 = 288.15    # K, sea-level standard temp
L = 0.0065     # K/m, tropospheric lapse rate
P11 = 226.32   # hPa, pressure at 11km (tropopause)
T11 = 216.65   # K, isothermal stratosphere temp
R = 8.31446    # J/(mol*K)
G = 9.80665    # m/s^2
M_AIR = 0.0289644  # kg/mol

SCALE_HEIGHT_STRATO = (R * T11) / (G * M_AIR)  # ~6341.6 m


def pressure_hpa_to_altitude_m(p_hpa: float) -> float:
    if p_hpa <= 0:
        return float("nan")
    if p_hpa >= P11:
        # Troposphere
        return (T0 / L) * (1 - (p_hpa / P0) ** ((R * L) / (G * M_AIR)))
    else:
        # Lower stratosphere, isothermal
        return 11000.0 + SCALE_HEIGHT_STRATO * math.log(P11 / p_hpa)


# ---------------------------------------------------------------------------
# Source resolution
# ---------------------------------------------------------------------------
# Resolving *which* file and *which* time step to serve is separate from
# reading it, so that a configuration mistake produces one clear explanation
# up front instead of an obscure failure three layers down.
#
# Precedence:
#   1. WIND_NETCDF_FILE           -- explicit override, catalog ignored
#   2. data/catalog.json          -- its "default" entry, or the sole usable one
#   3. synthetic analytic wind    -- so the server still starts, loudly
#
# Failing over to synthetic wind rather than refusing to start is deliberate.
# The alternative used to be worse than a dead server: sim-server fetches this
# endpoint exactly once at startup and falls back to WindField::zero() on any
# error with a single warn! line, so a config mistake surfaced as "the whole
# simulation runs, balloons just never move" -- easy to stare past for an hour.
# Now the problem travels *with the data*, in the response's "source" field.
_source_cache = {}


class SourceProblem(Exception):
    """A resolution failure worth explaining to the user, with the fix."""


def _resolve_stride():
    raw = os.environ.get("WIND_SPATIAL_STRIDE")
    if raw is None:
        return DEFAULT_SPATIAL_STRIDE
    try:
        stride = int(raw)
    except ValueError:
        raise SourceProblem(f"WIND_SPATIAL_STRIDE={raw!r} is not an integer.")
    if stride < 1:
        raise SourceProblem(f"WIND_SPATIAL_STRIDE={stride} must be >= 1.")
    return stride


def _open_and_describe(path):
    if not os.path.exists(path):
        raise SourceProblem(f"Data file does not exist: {path}")
    try:
        ds = xr.open_dataset(path)
    except Exception as e:
        raise SourceProblem(f"Could not open {path}: {type(e).__name__}: {e}")

    names, reasons = describe_wind_source(ds)
    if names is None:
        ds.close()
        raise SourceProblem(
            f"{path} is not usable as a pressure-level wind source:\n  - "
            + "\n  - ".join(reasons)
        )
    return ds, names


def _read_catalog():
    catalog_path = os.environ.get("WIND_DATA_CATALOG") or os.path.join(DATA_DIR, "catalog.json")
    if not os.path.exists(catalog_path):
        raise SourceProblem(
            f"No data catalog at {catalog_path}.\n"
            f"Generate one with:  {CATALOG_COMMAND}\n"
            f"(or set WIND_NETCDF_FILE=<path> to bypass the catalog entirely)"
        )
    try:
        with open(catalog_path) as fh:
            return catalog_path, json.load(fh)
    except Exception as e:
        raise SourceProblem(
            f"Could not parse the catalog at {catalog_path}: {e}\n"
            f"Regenerate it with:  {CATALOG_COMMAND}"
        )


def _pick_catalog_entry(catalog, catalog_path):
    files = catalog.get("files", [])
    usable = [f for f in files if f.get("usableAsWindSource")]

    default_path = catalog.get("default")
    if default_path:
        entry = next((f for f in files if f.get("path") == default_path), None)
        if entry is not None and entry.get("usableAsWindSource"):
            return entry

    if len(usable) == 1:
        _log(
            f"[source] catalog default {default_path!r} is unusable; falling back to the "
            f"only usable wind source: {usable[0]['path']}"
        )
        return usable[0]

    if not usable:
        raise SourceProblem(
            f"The catalog at {catalog_path} lists no usable wind source.\n"
            f"You need an ERA5 pressure-level file with u and v wind in {DATA_DIR}/.\n"
            f"See README.md for how to acquire one, then re-run:  {CATALOG_COMMAND}"
        )

    raise SourceProblem(
        f"The catalog at {catalog_path} has no usable default and "
        f"{len(usable)} usable candidates:\n  - "
        + "\n  - ".join(f["path"] for f in usable)
        + f"\nPick one with:  {CATALOG_COMMAND} --default <path>\n"
        f"(or set WIND_NETCDF_FILE=<path>)"
    )


def _check_staleness(entry, abs_path):
    """A catalog that no longer matches the disk is worth saying out loud, but
    not worth refusing to serve over -- the file is still readable and the
    parts we use (dims, levels, grid) are re-read from the dataset anyway."""
    try:
        size = os.path.getsize(abs_path)
    except OSError:
        return
    if entry.get("sizeBytes") not in (None, size):
        _log(
            f"[source] WARNING: catalog is stale for {entry['path']} "
            f"(recorded {entry['sizeBytes']} bytes, on disk {size}). "
            f"Re-run:  {CATALOG_COMMAND}"
        )


def _resolve_time_index(time_count, entry):
    raw = os.environ.get("WIND_TIME_INDEX")
    if raw is None:
        index = (entry or {}).get("defaultTimeIndex", 0) or 0
        origin = "default"
    else:
        try:
            index = int(raw)
        except ValueError:
            raise SourceProblem(f"WIND_TIME_INDEX={raw!r} is not an integer.")
        origin = "WIND_TIME_INDEX"

    if not 0 <= index < time_count:
        raise SourceProblem(
            f"Time index {index} (from {origin}) is out of range: this file has "
            f"{time_count} time step(s), so valid indices are 0..{time_count - 1}."
        )
    return index


def _resolve_source_uncached():
    stride = _resolve_stride()

    override = os.environ.get("WIND_NETCDF_FILE")
    if override:
        abs_path = override if os.path.isabs(override) else os.path.join(HERE, override)
        ds, names = _open_and_describe(abs_path)
        entry, catalog_path = None, None
    else:
        catalog_path, catalog = _read_catalog()
        entry = _pick_catalog_entry(catalog, catalog_path)
        abs_path = os.path.join(catalog.get("dataDir") or DATA_DIR, entry["path"])
        _check_staleness(entry, abs_path)
        ds, names = _open_and_describe(abs_path)

    time_values = np.asarray(ds[names["timeDim"]].values).ravel()
    time_count = len(time_values)
    if time_count == 0:
        ds.close()
        raise SourceProblem(f"{abs_path} has an empty time dimension.")

    try:
        time_index = _resolve_time_index(time_count, entry)
    except SourceProblem:
        ds.close()
        raise

    valid_time = np.datetime_as_string(np.datetime64(time_values[time_index]), unit="s").item()

    return {
        "ds": ds,
        "names": names,
        "meta": {
            "file": os.path.relpath(abs_path, HERE),
            "timeIndex": time_index,
            "validTime": valid_time,
            "timeStepCount": time_count,
            "spatialStride": stride,
            "synthetic": False,
            "problem": None,
            "catalog": os.path.relpath(catalog_path, HERE) if catalog_path else None,
        },
    }


def resolve_source():
    """Resolved source, cached for the process lifetime."""
    if "resolved" not in _source_cache:
        try:
            _source_cache["resolved"] = _resolve_source_uncached()
        except SourceProblem as e:
            _source_cache["resolved"] = {
                "ds": None,
                "names": None,
                "meta": {
                    "file": None,
                    "timeIndex": 0,
                    "validTime": None,
                    "timeStepCount": 1,
                    "spatialStride": None,
                    "synthetic": True,
                    "problem": str(e),
                    "catalog": None,
                },
            }
            _print_problem_banner(str(e))
    return _source_cache["resolved"]


def _print_problem_banner(message):
    bar = "=" * 78
    _log(f"\n{bar}")
    _log("WIND BACKEND: no real wind data -- SERVING SYNTHETIC WIND")
    _log(bar)
    _log(message)
    _log(bar)
    _log(
        "The server is up and every endpoint works, but the wind is an analytic\n"
        "jet-stream approximation, not reanalysis data. Responses are marked with\n"
        '  "source": {"synthetic": true, "problem": "..."}\n'
        "so the frontend can say so too."
    )
    _log(f"{bar}\n")


# ---------------------------------------------------------------------------
# Synthetic fallback field
# ---------------------------------------------------------------------------
# ERA5's standard 37 pressure levels, so a synthetic run exercises the same
# level-bracketing code paths as a real one.
SYNTHETIC_LEVELS_HPA = [
    1, 2, 3, 5, 7, 10, 20, 30, 50, 70, 100, 125, 150, 175, 200, 225, 250,
    300, 350, 400, 450, 500, 550, 600, 650, 700, 750, 775, 800, 825, 850,
    875, 900, 925, 950, 975, 1000,
]

SYNTHETIC_GRID_STEP_DEG = 5.0
SYNTHETIC_JET_PEAK_HPA = 225.0   # where the jet maximum sits
SYNTHETIC_JET_MAX_MS = 40.0      # peak westerly speed
SYNTHETIC_JET_LAT_DEG = 45.0     # mid-latitude jet core
SYNTHETIC_JET_LAT_WIDTH = 15.0   # Gaussian half-width in latitude


def _build_synthetic_response(problem):
    """An analytic mid-latitude westerly jet.

    Not an attempt at realism -- just a field with the right gross structure
    (westerlies peaking near 200-250hPa around 45N/45S, calm at the surface and
    at the top of the atmosphere, plus a gentle meridional wave so it isn't
    purely zonal) so that a run without real data still looks like weather and
    still stresses vertical interpolation.
    """
    lats = np.arange(90.0, -90.0 - SYNTHETIC_GRID_STEP_DEG / 2, -SYNTHETIC_GRID_STEP_DEG)
    lons = np.arange(0.0, 360.0, SYNTHETIC_GRID_STEP_DEG)

    header = {
        "nx": len(lons),
        "ny": len(lats),
        "lo1": float(lons.min()),
        "la1": float(lats.max()),
        "lo2": float(lons.max()),
        "la2": float(lats.min()),
        "dx": SYNTHETIC_GRID_STEP_DEG,
        "dy": SYNTHETIC_GRID_STEP_DEG,
    }

    lat_grid, lon_grid = np.meshgrid(lats, lons, indexing="ij")
    # Two jet cores, one per hemisphere, symmetric about the equator.
    lat_envelope = np.exp(-(((np.abs(lat_grid) - SYNTHETIC_JET_LAT_DEG) / SYNTHETIC_JET_LAT_WIDTH) ** 2))

    levels_payload = []
    for p_hpa in SYNTHETIC_LEVELS_HPA:
        # Log-pressure Gaussian: broad in altitude, peaked at the jet level.
        vertical = math.exp(-((math.log(p_hpa / SYNTHETIC_JET_PEAK_HPA) / 0.9) ** 2))
        u = SYNTHETIC_JET_MAX_MS * vertical * lat_envelope
        v = 5.0 * vertical * lat_envelope * np.sin(np.deg2rad(4.0 * lon_grid))
        levels_payload.append(
            {
                "pressureHpa": float(p_hpa),
                "altitudeM": pressure_hpa_to_altitude_m(float(p_hpa)),
                "u_data": u.tolist(),
                "v_data": v.tolist(),
            }
        )

    levels_payload.sort(key=lambda lvl: lvl["altitudeM"])

    return {
        "header": header,
        "levels": levels_payload,
        "source": {
            "file": None,
            "timeIndex": 0,
            "validTime": None,
            "timeStepCount": 1,
            "spatialStride": None,
            "synthetic": True,
            "problem": problem,
            "catalog": None,
        },
    }


# ---------------------------------------------------------------------------
# App
# ---------------------------------------------------------------------------
app = FastAPI()

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)

_wind_levels_response_cache = {}


def _snapshot():
    """The single pinned time step, strided, with resolved dim names."""
    source = resolve_source()
    ds, names, meta = source["ds"], source["names"], source["meta"]

    snapshot = ds.isel({names["timeDim"]: meta["timeIndex"]})
    stride = meta["spatialStride"]
    snapshot = snapshot.isel(
        {
            names["latDim"]: slice(None, None, stride),
            names["lonDim"]: slice(None, None, stride),
        }
    )
    return snapshot, names, meta


def _build_wind_levels_response():
    """Builds the /api/wind-levels payload, then caches it for the process
    lifetime.

    The source file may hold many time steps -- the 24-hour ERA5 downloads do --
    but exactly one is pinned at startup (see resolve_source) and only that one
    is ever built here. That is a size decision, not a data one: a single
    stride-2 step is ~350MB of JSON taking ~55s to serialize (see
    docs/investigations/WIND_TRANSFER_PERF.md), so caching several would
    exhaust memory and rebuilding on demand would stall every request. Use
    WIND_TIME_INDEX to serve a different hour, or WIND_SPATIAL_STRIDE to trade
    resolution for speed.
    """
    snapshot, names, meta = _snapshot()

    lats = snapshot[names["latDim"]].values.tolist()
    lons = snapshot[names["lonDim"]].values.tolist()
    levels_hpa = snapshot[names["levelDim"]].values.tolist()

    header = {
        "nx": len(lons),
        "ny": len(lats),
        "lo1": min(lons),
        "la1": max(lats),
        "lo2": max(lons),
        "la2": min(lats),
        "dx": abs(lons[1] - lons[0]) if len(lons) > 1 else 1,
        "dy": abs(lats[1] - lats[0]) if len(lats) > 1 else 1,
    }

    levels_payload = []
    for p_hpa in levels_hpa:
        level_slice = snapshot.sel({names["levelDim"]: p_hpa})
        u_vals = np.nan_to_num(level_slice[names["uVar"]].values).tolist()
        v_vals = np.nan_to_num(level_slice[names["vVar"]].values).tolist()
        levels_payload.append(
            {
                "pressureHpa": float(p_hpa),
                "altitudeM": pressure_hpa_to_altitude_m(float(p_hpa)),
                "u_data": u_vals,
                "v_data": v_vals,
            }
        )

    # Sort levels by altitude ascending -- makes bracketing-level lookup
    # on the frontend simpler (walk up the list until altitude brackets).
    # sim-server's WindField::sample depends on this ordering.
    levels_payload.sort(key=lambda lvl: lvl["altitudeM"])

    # "source" is additive: a sibling of header/levels, never a change to
    # either. sim-server's serde structs ignore unknown fields but reject
    # renamed or missing ones (falling back to zero wind with only a warning),
    # so adding keys is safe and touching the existing ones is not.
    return {
        "header": header,
        "levels": levels_payload,
        "source": dict(meta),
    }


def get_wind_levels_cached():
    if "response" not in _wind_levels_response_cache:
        meta = resolve_source()["meta"]
        if meta["synthetic"]:
            _wind_levels_response_cache["response"] = _build_synthetic_response(meta["problem"])
        else:
            _wind_levels_response_cache["response"] = _build_wind_levels_response()
    return _wind_levels_response_cache["response"]


@app.on_event("startup")
def preload_dataset():
    # Resolve and build up front, so problems are visible in the uvicorn log
    # immediately rather than on the first request.
    meta = resolve_source()["meta"]
    if not meta["synthetic"]:
        _log(
            f"[source] serving {meta['file']}  timeIndex={meta['timeIndex']} "
            f"validTime={meta['validTime']} of {meta['timeStepCount']} step(s)  "
            f"stride={meta['spatialStride']}  (via {meta['catalog'] or 'WIND_NETCDF_FILE'})"
        )
    try:
        get_wind_levels_cached()
    except Exception as e:
        # Don't crash the whole app import; let the endpoint report it too,
        # but this makes the problem visible in the uvicorn log immediately.
        _log(f"[startup] WARNING: failed to build wind payload: {e}")


@app.get("/api/wind-levels")
def get_wind_levels():
    try:
        return get_wind_levels_cached()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to build wind payload: {e}")


@app.get("/api/wind-levels/source")
def get_wind_levels_source():
    """Which file, time step and stride are actually being served.

    Cheap by design -- it never touches the grids, so it's the endpoint to hit
    when you want to know what the server resolved without paying for the
    ~350MB payload.
    """
    meta = dict(resolve_source()["meta"])
    meta["catalogCommand"] = CATALOG_COMMAND
    return meta


@app.get("/api/wind-levels/stats")
def get_wind_levels_stats():
    """Debug endpoint: min/max/mean wind speed per level, without shipping
    the full grid. Use this to sanity-check magnitudes (e.g. confirm the
    jet stream shows up as a speed spike around 200-300hPa)."""
    source = resolve_source()
    if source["meta"]["synthetic"]:
        # Tiny grid, so just derive the stats from the built payload.
        payload = get_wind_levels_cached()
        stats = []
        for lvl in payload["levels"]:
            speed = np.hypot(np.asarray(lvl["u_data"]), np.asarray(lvl["v_data"]))
            stats.append(
                {
                    "pressureHpa": lvl["pressureHpa"],
                    "altitudeM": lvl["altitudeM"],
                    "speedMinMs": float(speed.min()),
                    "speedMaxMs": float(speed.max()),
                    "speedMeanMs": float(speed.mean()),
                }
            )
        return {"levels": stats, "source": payload["source"]}

    # Same pinned time step as /api/wind-levels -- reading a different hour
    # here than the payload serves would make this actively misleading.
    snapshot, names, meta = _snapshot()
    levels_hpa = snapshot[names["levelDim"]].values.tolist()

    stats = []
    for p_hpa in levels_hpa:
        level_slice = snapshot.sel({names["levelDim"]: p_hpa})
        u = np.nan_to_num(level_slice[names["uVar"]].values)
        v = np.nan_to_num(level_slice[names["vVar"]].values)
        speed = np.sqrt(u**2 + v**2)
        stats.append(
            {
                "pressureHpa": float(p_hpa),
                "altitudeM": pressure_hpa_to_altitude_m(float(p_hpa)),
                "speedMinMs": float(speed.min()),
                "speedMaxMs": float(speed.max()),
                "speedMeanMs": float(speed.mean()),
            }
        )

    stats.sort(key=lambda lvl: lvl["altitudeM"])
    return {"levels": stats, "source": dict(meta)}


@app.get("/api/wind-levels/meta")
def get_wind_levels_meta():
    """Lightweight endpoint: just the available pressure levels and their
    approximate altitudes, without the full grid payload. Useful for
    sanity-checking the altitude conversion against your file's levels."""
    source = resolve_source()
    if source["meta"]["synthetic"]:
        levels_hpa = [float(p) for p in SYNTHETIC_LEVELS_HPA]
    else:
        levels_hpa = source["ds"][source["names"]["levelDim"]].values.tolist()

    return {
        "levels": sorted(
            [
                {"pressureHpa": float(p), "altitudeM": pressure_hpa_to_altitude_m(float(p))}
                for p in levels_hpa
            ],
            key=lambda x: x["altitudeM"],
        ),
        "source": dict(source["meta"]),
    }

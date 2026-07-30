#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
FastAPI backend serving multi-level (pressure-level) wind data parsed from an
ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    ./run.sh

Which file gets served is read from `wind_source.json` (edit it by hand) --
there is no hardcoded filename. Run `catalog_data.py` to see what's in `data/`.
See README.md.

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.
"""

import gzip
import json
import math
import os
import sys

import numpy as np
import xarray as xr
from fastapi import FastAPI, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import Response

from wind_data import describe_wind_source

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
# No hardcoded data filename: Copernicus downloads arrive named after an opaque
# request hash, so the file to serve lives in a small hand-edited JSON file
# instead of in this source.
HERE = os.path.dirname(os.path.abspath(__file__))
DATA_DIR = os.path.join(HERE, "data")
SOURCE_CONFIG = os.path.join(HERE, "wind_source.json")

DEFAULT_SPATIAL_STRIDE = 2  # downsample every Nth grid point in lat/lon
CATALOG_COMMAND = "./.venv/bin/python catalog_data.py"

# The source data is float32 (~7 significant decimal digits). Converting a
# float32 array straight to a Python list makes each value a float64 that
# reproduces the float32 bit pattern *exactly* -- e.g. 12.4f32 becomes
# 12.399999618530273 -- and json.dumps then prints that float64's full
# shortest round-trip repr, ~17 digits of text for ~7 digits of real
# precision. Rounding to this many decimal places after casting to float64
# discards that upscaled noise without discarding any real precision (wind
# speeds here range roughly +/-100 m/s, so 4 decimals is well past ERA5's
# actual resolution). Must cast to float64 *before* rounding: rounding a
# float32 array leaves it float32, and converting that back to float64
# reintroduces the same binary noise.
WIND_VALUE_DECIMALS = 4


def _round_wind_values(arr):
    return np.round(arr.astype(np.float64), WIND_VALUE_DECIMALS)


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
# Falling back to synthetic wind rather than refusing to start is deliberate.
# The alternative is worse than a dead server: sim-server fetches this endpoint
# exactly once at startup and falls back to WindField::zero() on any error with
# a single warn! line, so a misconfiguration surfaces as "the whole simulation
# runs, balloons just never move" -- a symptom that looks nothing like its
# cause. Serving a plausible field and marking it keeps the app usable and
# carries the explanation with the data, in the response's "source" field.
_source_cache = {}


class SourceProblem(Exception):
    """A resolution failure worth explaining to the user, with the fix."""


def _read_source_config():
    if not os.path.exists(SOURCE_CONFIG):
        raise SourceProblem(
            f"No source config at {SOURCE_CONFIG}.\n"
            f"Copy wind_source_template.json to wind_source.json and edit "
            f'"file" to point at your own data. See README.md.'
        )
    try:
        with open(SOURCE_CONFIG) as fh:
            config = json.load(fh)
    except Exception as e:
        raise SourceProblem(f"Could not parse {SOURCE_CONFIG}: {e}")
    if not isinstance(config, dict):
        raise SourceProblem(f"{SOURCE_CONFIG} must contain a JSON object.")
    return config


def _resolve_int(config, key, default, minimum=None):
    value = config.get(key, default)
    if value is None:
        value = default
    if not isinstance(value, int) or isinstance(value, bool):
        raise SourceProblem(f'"{key}" in {SOURCE_CONFIG} must be an integer, got {value!r}.')
    if minimum is not None and value < minimum:
        raise SourceProblem(f'"{key}" in {SOURCE_CONFIG} must be >= {minimum}, got {value}.')
    return value


def _resolve_source_uncached():
    config = _read_source_config()

    raw_path = config.get("file")
    if not raw_path:
        raise SourceProblem(
            f'"file" is not set in {SOURCE_CONFIG}.\n'
            f"Put an ERA5 pressure-level NetCDF file in {DATA_DIR}/, then run:\n"
            f"  {CATALOG_COMMAND}\n"
            f'and copy the path it suggests into the "file" field.'
        )

    abs_path = raw_path if os.path.isabs(raw_path) else os.path.join(DATA_DIR, raw_path)
    if not os.path.exists(abs_path):
        raise SourceProblem(
            f'"file" in {SOURCE_CONFIG} points at a file that does not exist:\n'
            f"  {abs_path}\n"
            f"Paths are relative to {DATA_DIR}/. Run {CATALOG_COMMAND} to list what's there."
        )

    stride = _resolve_int(config, "spatialStride", DEFAULT_SPATIAL_STRIDE, minimum=1)

    try:
        ds = xr.open_dataset(abs_path)
    except Exception as e:
        raise SourceProblem(f"Could not open {abs_path}: {type(e).__name__}: {e}")

    names, reasons = describe_wind_source(ds)
    if names is None:
        ds.close()
        raise SourceProblem(
            f"{abs_path} is not usable as a pressure-level wind source:\n  - "
            + "\n  - ".join(reasons)
        )

    time_values = np.asarray(ds[names["timeDim"]].values).ravel()
    time_count = len(time_values)
    if time_count == 0:
        ds.close()
        raise SourceProblem(f"{abs_path} has an empty time dimension.")

    try:
        time_index = _resolve_int(config, "timeIndex", 0, minimum=0)
        if time_index >= time_count:
            raise SourceProblem(
                f'"timeIndex": {time_index} in {SOURCE_CONFIG} is out of range: this file '
                f"has {time_count} time step(s), so valid indices are 0..{time_count - 1}."
            )
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
                "u_data": _round_wind_values(u).tolist(),
                "v_data": _round_wind_values(v).tolist(),
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
    """The single selected time step, strided, with resolved dim names."""
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

    The source file may hold more than one time step, but only the one selected
    in wind_source.json is ever built here. That's a size decision: a single
    stride-2 step is ~350MB of JSON taking ~55s to serialize (see
    docs/investigations/WIND_TRANSFER_PERF.md), so caching several would exhaust
    memory. Change "timeIndex" or "spatialStride" in wind_source.json and
    restart to serve something different.
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

    # Each iteration's .sel() + [...].values is a separate netCDF disk read --
    # 37 levels * 2 vars = 74 reads, ~0.35s apiece, ~28.9s total measured via
    # cProfile (all in netCDF4_.py:_getitem -> _operator.getitem). That's 62%
    # of the ~46s startup pipeline and the single biggest cost in it -- bigger
    # than json.dumps and gzip combined. Root cause is how the source file is
    # chunked (see wind_backend_perf notes); reading the whole u/v variable
    # once instead of per-level slices is the lever, not anything below here.
    levels_payload = []
    for p_hpa in levels_hpa:
        level_slice = snapshot.sel({names["levelDim"]: p_hpa})
        u_vals = _round_wind_values(np.nan_to_num(level_slice[names["uVar"]].values)).tolist()
        v_vals = _round_wind_values(np.nan_to_num(level_slice[names["vVar"]].values)).tolist()
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


# The payload never changes after startup (one time step, resolved once), but
# FastAPI's default handling re-runs jsonable_encoder + json.dumps over the
# full nested-float dict on *every* request -- measured at ~17.7s/request
# against the cached dict alone, identical on repeat requests, confirming the
# encode pass (not the dict build) was the recurring cost. So encode once here
# -- both plain and gzipped, since the client is a one-shot fetch-at-startup
# (sim-server) rather than a hot path worth compressing per-request -- and
# have the endpoint serve pre-made bytes instead of a dict.
_wind_levels_bytes_cache = {}


def _wind_levels_json_bytes():
    if "raw" not in _wind_levels_bytes_cache:
        # Compact separators -- FastAPI's default JSONResponse uses these too;
        # json.dumps()'s own default (", ", ": ") would add a wasted space
        # after every one of the ~19M numbers in this payload.
        raw = json.dumps(get_wind_levels_cached(), separators=(",", ":")).encode("utf-8")
        _wind_levels_bytes_cache["raw"] = raw
        # Level 1, not the zlib default of 6: profiling showed compresslevel=6
        # taking ~11.1s of the ~46s startup pipeline (second only to the
        # netCDF reads) to compress a payload nothing currently asks for --
        # sim-server's reqwest client doesn't send Accept-Encoding: gzip (see
        # README). Cheap insurance for a client that does ask, not worth
        # spending real startup time optimizing the ratio.
        _wind_levels_bytes_cache["gzip"] = gzip.compress(raw, compresslevel=1)
    return _wind_levels_bytes_cache["raw"], _wind_levels_bytes_cache["gzip"]


@app.on_event("startup")
def preload_dataset():
    # Resolve and build up front, so problems are visible in the uvicorn log
    # immediately rather than on the first request.
    meta = resolve_source()["meta"]
    if not meta["synthetic"]:
        _log(
            f"[source] serving {meta['file']}  timeIndex={meta['timeIndex']} "
            f"validTime={meta['validTime']} of {meta['timeStepCount']} step(s)  "
            f"stride={meta['spatialStride']}"
        )
    try:
        get_wind_levels_cached()
        _wind_levels_json_bytes()
    except Exception as e:
        # Don't crash the whole app import; let the endpoint report it too,
        # but this makes the problem visible in the uvicorn log immediately.
        _log(f"[startup] WARNING: failed to build wind payload: {e}")


@app.get("/api/wind-levels")
def get_wind_levels(request: Request):
    try:
        raw_bytes, gzip_bytes = _wind_levels_json_bytes()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to build wind payload: {e}")

    accept_encoding = request.headers.get("accept-encoding", "")
    if "gzip" in accept_encoding:
        return Response(
            content=gzip_bytes,
            media_type="application/json",
            headers={"Content-Encoding": "gzip", "Vary": "Accept-Encoding"},
        )
    return Response(content=raw_bytes, media_type="application/json")


@app.get("/api/wind-levels/source")
def get_wind_levels_source():
    """Which file and time step are actually being served, and whether the wind
    is real or synthetic.

    Cheap by design -- it never touches the grids, so it's the endpoint to hit
    when you want to know what the server resolved without paying for the
    ~350MB payload.
    """
    meta = dict(resolve_source()["meta"])
    meta["config"] = os.path.relpath(SOURCE_CONFIG, HERE)
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

    # Same time step as /api/wind-levels -- reading a different one here than
    # the payload serves would make this actively misleading.
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

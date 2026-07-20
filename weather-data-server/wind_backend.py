#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
FastAPI backend serving multi-level (pressure-level) wind data parsed from a
static ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    conda activate reginald
    uvicorn wind_backend:app --reload

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.
"""

from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
import xarray as xr
import numpy as np
import math

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
#NETCDF_FILE = "data/era5_pressure_levels.nc"  # <-- point this at your file
NETCDF_FILE = "data/67e68497a30d3b5946ac20a319b63fd4.nc"
SPATIAL_STRIDE = 2  # downsample every Nth grid point in lat/lon, like before

# ERA5 pressure-level variable names are usually 'u' and 'v'. Some exports
# use 'u_component_of_wind' / 'v_component_of_wind'. We try both.
U_VAR_CANDIDATES = ["u", "u_component_of_wind"]
V_VAR_CANDIDATES = ["v", "v_component_of_wind"]
LEVEL_DIM_CANDIDATES = ["pressure_level", "level", "isobaricInhPa"]
TIME_DIM_CANDIDATES = ["valid_time", "time"]

# ---------------------------------------------------------------------------
# ISA (International Standard Atmosphere) pressure -> geometric altitude.
# Piecewise: troposphere (0-11km) + lower stratosphere isothermal layer
# (11-20km). This is an approximation -- fine for a simulator, not for
# real navigation. Balloon altitudes of interest (15-25km) sit mostly in
# the isothermal layer; error grows a bit above ~20km where the real
# atmosphere's lapse rate changes again, but stays close enough for this
# use case.
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
# Dataset loading (once, cached at module scope -- not reopened per request)
# ---------------------------------------------------------------------------
_dataset_cache = {}


def _pick_var(ds, candidates, kind):
    for name in candidates:
        if name in ds.variables:
            return name
    raise RuntimeError(
        f"Could not find a {kind} variable in the dataset. "
        f"Looked for: {candidates}. Available: {list(ds.variables)}"
    )


def _pick_dim(ds, candidates, kind):
    for name in candidates:
        if name in ds.dims:
            return name
    raise RuntimeError(
        f"Could not find a {kind} dimension in the dataset. "
        f"Looked for: {candidates}. Available: {list(ds.dims)}"
    )


def get_dataset():
    if "ds" not in _dataset_cache:
        ds = xr.open_dataset(NETCDF_FILE)
        _dataset_cache["ds"] = ds
        _dataset_cache["u_var"] = _pick_var(ds, U_VAR_CANDIDATES, "u-wind")
        _dataset_cache["v_var"] = _pick_var(ds, V_VAR_CANDIDATES, "v-wind")
        _dataset_cache["level_dim"] = _pick_dim(ds, LEVEL_DIM_CANDIDATES, "pressure level")
        _dataset_cache["time_dim"] = _pick_dim(ds, TIME_DIM_CANDIDATES, "time")
    return _dataset_cache


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


@app.on_event("startup")
def preload_dataset():
    # Fail fast at startup if the file/variable names don't match, rather
    # than on the first request.
    try:
        get_dataset()
    except Exception as e:
        # Don't crash the whole app import; let the endpoint report it too,
        # but this makes the problem visible in the uvicorn log immediately.
        print(f"[startup] WARNING: failed to load dataset: {e}")


@app.get("/api/wind-levels")
def get_wind_levels():
    try:
        cache = get_dataset()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to open dataset: {e}")

    ds = cache["ds"]
    u_var, v_var = cache["u_var"], cache["v_var"]
    level_dim, time_dim = cache["level_dim"], cache["time_dim"]

    # Single static time snapshot: first time step.
    snapshot = ds.isel({time_dim: 0})

    # Downsample lat/lon for browser payload size.
    lat_dim = "latitude" if "latitude" in snapshot.dims else "lat"
    lon_dim = "longitude" if "longitude" in snapshot.dims else "lon"
    snapshot = snapshot.isel(
        {
            lat_dim: slice(None, None, SPATIAL_STRIDE),
            lon_dim: slice(None, None, SPATIAL_STRIDE),
        }
    )

    lats = snapshot[lat_dim].values.tolist()
    lons = snapshot[lon_dim].values.tolist()
    levels_hpa = snapshot[level_dim].values.tolist()

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
        level_slice = snapshot.sel({level_dim: p_hpa})
        u_vals = np.nan_to_num(level_slice[u_var].values).tolist()
        v_vals = np.nan_to_num(level_slice[v_var].values).tolist()
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
    levels_payload.sort(key=lambda lvl: lvl["altitudeM"])

    return {
        "header": header,
        "levels": levels_payload,
    }


@app.get("/api/wind-levels/stats")
def get_wind_levels_stats():
    """Debug endpoint: min/max/mean wind speed per level, without shipping
    the full grid. Use this to sanity-check magnitudes (e.g. confirm the
    jet stream shows up as a speed spike around 200-300hPa)."""
    try:
        cache = get_dataset()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to open dataset: {e}")

    ds = cache["ds"]
    u_var, v_var = cache["u_var"], cache["v_var"]
    level_dim, time_dim = cache["level_dim"], cache["time_dim"]

    snapshot = ds.isel({time_dim: 0})
    levels_hpa = snapshot[level_dim].values.tolist()

    stats = []
    for p_hpa in levels_hpa:
        level_slice = snapshot.sel({level_dim: p_hpa})
        u = np.nan_to_num(level_slice[u_var].values)
        v = np.nan_to_num(level_slice[v_var].values)
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
    return {"levels": stats}


@app.get("/api/wind-levels/meta")
def get_wind_levels_meta():
    """Lightweight endpoint: just the available pressure levels and their
    approximate altitudes, without the full grid payload. Useful for
    sanity-checking the altitude conversion against your file's levels."""
    try:
        cache = get_dataset()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to open dataset: {e}")

    ds = cache["ds"]
    level_dim = cache["level_dim"]
    levels_hpa = ds[level_dim].values.tolist()
    return {
        "levels": sorted(
            [
                {"pressureHpa": float(p), "altitudeM": pressure_hpa_to_altitude_m(float(p))}
                for p in levels_hpa
            ],
            key=lambda x: x["altitudeM"],
        )
    }

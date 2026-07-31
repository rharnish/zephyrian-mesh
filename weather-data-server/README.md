FastAPI backend serving multi-level (pressure-level) wind data parsed from a
static ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    ./run.sh

`run.sh` uses a minimal venv local to this directory (`.venv/`) — creates
it and installs `requirements.txt` automatically on first run.

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.

## Data

`data/` (the ERA5 NetCDF/GRIB files `wind_backend.py` reads) is **not
committed to git** — it's ~1GB, over GitHub's per-file size limit, and not
really source anyway. `.gitignore` excludes `weather-data-server/data/`
entirely. If you're setting this up somewhere new, you'll need to source
your own ERA5 pressure-level data (e.g. from the Copernicus Climate Data
Store) and place it there, then point `NETCDF_FILE` in `wind_backend.py`
at it.


FastAPI backend serving multi-level (pressure-level) wind data parsed from an
ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    ./run.sh

`run.sh` uses a minimal venv local to this directory (`.venv/`, not the
`reginald` conda env) — creates it and installs `requirements.txt`
automatically on first run. Prefer conda instead? This still works the
same way:
    conda activate reginald
    uvicorn wind_backend:app --reload

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.

## Data

`data/` (the ERA5 NetCDF/GRIB files this server reads) is **not committed to
git** — the files run from hundreds of MB to ~8GB, well over GitHub's per-file
limit, and they aren't source anyway. `.gitignore` excludes
`weather-data-server/data/` entirely, `data/catalog.json` included.

So on a fresh clone there is no wind data. See
[Acquiring your own data](#acquiring-your-own-data) below.

### Cataloging what you have

There is no hardcoded filename in `wind_backend.py`. Copernicus names your
download after an opaque request hash (`b2436709633b357f9ec6d77da9772ea0.nc`),
which tells you nothing about what's inside, so instead a catalog script
inventories `data/` and the server reads its output:

```bash
./.venv/bin/python catalog_data.py            # writes data/catalog.json
./.venv/bin/python catalog_data.py --print    # look without writing
```

It opens every `.nc`/`.grib` under `data/` (recursively — `.zip` downloads
extract into a subdirectory) and reports, per file: the time steps and how they
group into **contiguous spans**, the pressure levels, the grid geometry, an
estimated payload size, and whether the file is usable as a wind source at all.
It reads **metadata only** — never the wind arrays — so cataloging an 8GB file
takes under a second.

Sample output:

```
OK b2436709633b357f9ec6d77da9772ea0.nc  (7.47 GB)  <- default
      time:    24 step(s); 1978-06-09T00:00:00 -> 1978-06-09T23:00:00 (24 @ 1h)
      levels:  37 (1000 -> 1 hPa)
      grid:    721x1440 @ 0.25deg, lat 90->-90, lon 0->359.75
      payload: stride 2 -> 361x720, ~352.2 MB JSON / ~73.4 MB f32 per time step
-- 4bd6338f235b60709d9140c9abfa0ade/data_stream-oper_stepType-instant.nc  (0.01 GB)
      not a wind source: no pressure-level dim (looked for pressure_level, level, isobaricInhPa)
```

The server serves the catalog's `default` entry. If several files are usable,
the script picks the one with the most time steps and writes down *why* in
`defaultReason`; pin a different one with `--default <path>`.

**Which file/hour is actually being served?** Ask the server — it's cheap, and
never touches the grids:

```bash
curl -s http://127.0.0.1:8000/api/wind-levels/source | python3 -m json.tool
```

### Overrides

| Variable | Default | Effect |
|---|---|---|
| `WIND_NETCDF_FILE` | *(unset)* | Serve this file directly, ignoring the catalog. The escape hatch for one-off experiments. |
| `WIND_DATA_CATALOG` | `data/catalog.json` | Read the catalog from somewhere else. |
| `WIND_TIME_INDEX` | `0` | Which time step to serve, `0`-based. Out-of-range tells you the valid range. |
| `WIND_SPATIAL_STRIDE` | `2` | Keep every Nth grid point in lat/lon. Raise it to trade resolution for speed — `8` turns a ~350MB payload into ~22MB. |

All are read once at startup, so changing one means restarting the server.

### When something is wrong

The server does **not** refuse to start if it can't find data. It serves an
analytic mid-latitude jet instead (westerlies peaking near 225hPa around
45°N/45°S), prints a loud banner explaining what went wrong, and marks every
response:

```json
"source": {"synthetic": true, "problem": "No data catalog at data/catalog.json. Generate one with: ..."}
```

That's deliberate. `sim-server` fetches this endpoint exactly once at startup
and falls back to `WindField::zero()` on any error with a single `warn!` line,
so a configuration mistake used to surface as "the whole simulation runs,
balloons just never move" — a symptom that looks nothing like its cause.
Synthetic wind keeps the app usable and carries the explanation with the data.

### What the server needs from a file

- `u` and `v` wind variables (or `u_component_of_wind` / `v_component_of_wind`)
- indexed by a **pressure-level** dim (`pressure_level`, `level`, or
  `isobaricInhPa`), plus time, latitude and longitude
- a **uniform** lat/lon grid — both `sim-server`'s sampler and the browser's
  arrow overlay reconstruct coordinates arithmetically (`lon = lo1 + col*dx`),
  so a non-uniform grid is silently misplaced rather than rejected. The catalog
  flags this as `grid.uniform`.

ERA5 **single-level** products (10m/100m winds) do *not* qualify — no pressure
levels, so no vertical structure for balloons to fly through. The catalog will
tell you so rather than letting you find out from a stack trace.

GRIB files need the `cfgrib` engine, which is deliberately **not** in
`requirements.txt` — `pip install cfgrib` if you want them cataloged.

Size reality, for a global 0.25° 37-level request:

| | size |
|---|---|
| one hour, on disk | ~850 MB |
| 24 hours, on disk | ~8 GB |
| one hour as JSON over the wire, stride 2 | ~350 MB / ~55 s |

See [`../docs/investigations/WIND_TRANSFER_PERF.md`](../docs/investigations/WIND_TRANSFER_PERF.md).
If that's more than you want: request fewer pressure levels, add an `area`
subset, ask for fewer hours, or raise `WIND_SPATIAL_STRIDE`.

Finally: **keep the `receipt-*.txt` Copernicus gives you next to the download.**
The catalog joins receipts to data files by the hash in the receipt's
`filename` field, which is the only way to recover what you actually asked for
— dataset, date, variables, licence — from a filename that's just a hash.

## Acquiring your own data

The data is ERA5 reanalysis from the Copernicus Climate Data Store.

1. Register at <https://cds.climate.copernicus.eu/> and get your API key from
   your profile page.
2. Open the
   [ERA5 pressure levels dataset page](https://cds.climate.copernicus.eu/datasets/reanalysis-era5-pressure-levels)
   and **accept the licence** under its Download tab. Requests fail until you
   do this once, and the error doesn't always make the reason obvious.
3. Put your key in `~/.cdsapirc`:

   ```
   url: https://cds.climate.copernicus.eu/api
   key: <your-key>
   ```

4. `pip install cdsapi`. It's an acquisition tool, not a server dependency, so
   it's deliberately not in `requirements.txt`.

Then request the data. This asks for a modest slice — the 50–200hPa band is
where the balloons actually fly, and a full 24 hours gives a contiguous span to
interpolate across:

```python
import cdsapi

cdsapi.Client().retrieve(
    "reanalysis-era5-pressure-levels",
    {
        "product_type": ["reanalysis"],
        "variable": ["u_component_of_wind", "v_component_of_wind"],
        "pressure_level": ["50", "70", "100", "125", "150", "175", "200", "250"],
        "year": ["2026"],
        "month": ["06"],
        "day": ["09"],
        "time": [f"{h:02d}:00" for h in range(24)],
        "data_format": "netcdf",
        "download_format": "unarchived",
    },
    "data/era5-pl-2026-06-09.nc",
)
```

Then `./.venv/bin/python catalog_data.py` and `./run.sh`.

Two things to watch:

- **The CDS API changes its key spellings occasionally** (`data_format` was
  `format`; `download_format` is newer still). If the request errors on a
  keyword, copy the auto-generated snippet from the dataset page's Download
  tab — that's always current — and keep `data_format: netcdf`.
- **ERA5 before 1979** has historically lived in a separate "preliminary back
  extension" dataset rather than the main one. If you want a pre-1979 date, you
  may need a different collection id.

<details>
<summary>The full-fidelity request (what's on the maintainer's disk, ~8 GB)</summary>

Global, 0.25°, all 37 pressure levels, all 24 hours. Everything below is
reconstructed from the on-disk file's own metadata, so it reproduces it exactly
— but it is an 8 GB download and the server only reads `u` and `v` from it.

```python
import cdsapi

cdsapi.Client().retrieve(
    "reanalysis-era5-pressure-levels",
    {
        "product_type": ["reanalysis"],
        # The file on disk also carries r, t and w — unused by this server:
        #   "relative_humidity", "temperature", "vertical_velocity"
        "variable": ["u_component_of_wind", "v_component_of_wind"],
        "pressure_level": [
            "1", "2", "3", "5", "7", "10", "20", "30", "50", "70", "100",
            "125", "150", "175", "200", "225", "250", "300", "350", "400",
            "450", "500", "550", "600", "650", "700", "750", "775", "800",
            "825", "850", "875", "900", "925", "950", "975", "1000",
        ],
        "year": ["2026"], "month": ["06"], "day": ["09"],
        "time": [f"{h:02d}:00" for h in range(24)],
        # No "area" key — omitting it gives the global 721x1440 grid.
        "data_format": "netcdf",
        "download_format": "unarchived",
    },
    "data/era5-pl-full.nc",
)
```

</details>

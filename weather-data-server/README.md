FastAPI backend serving multi-level (pressure-level) wind data parsed from an
ERA5 NetCDF file, for the balloon mesh simulator frontend.

TO START SERVER:
    ./run.sh

`run.sh` uses a minimal venv local to this directory (`.venv/`) — creates
it and installs `requirements.txt` automatically on first run.

Frontend usage: GET /api/wind-levels returns wind on every available
pressure level, each converted to an approximate altitude in meters, so the
client can pick the two bracketing levels for a balloon's current altitude
and interpolate.

## Data

`data/` (the ERA5 NetCDF/GRIB files this server reads) is **not committed to
git** — the files run from hundreds of MB up, over GitHub's per-file limit, and
they aren't source anyway. `.gitignore` excludes `weather-data-server/data/`
entirely.

So on a fresh clone there is no wind data. See
[Acquiring your own data](#acquiring-your-own-data) below.

### Pointing the server at a file

There's no hardcoded filename. Copernicus names your download after an opaque
request hash (`b2436709633b357f9ec6d77da9772ea0.nc`), so the file to serve lives
in **`wind_source.json`** — copy `wind_source_template.json` to
`wind_source.json` and edit it by hand:

```bash
cp wind_source_template.json wind_source.json
```

```json
{
  "file": "b2436709633b357f9ec6d77da9772ea0.nc",
  "timeIndex": 0,
  "spatialStride": 2
}
```

`wind_source.json` is gitignored — it names *your* local file, which won't
exist on anyone else's clone. `wind_source_template.json` is the committed
placeholder (`"file": null`, i.e. synthetic wind until you point it
somewhere real), the same pattern as `src/user-config-template.json` at the
repo root.

- **`file`** — path to a NetCDF file, relative to `data/` (or absolute).
  `null` means "no data": the server serves synthetic wind and says so.
- **`timeIndex`** — which time step to serve, 0-based, if the file has more
  than one.
- **`spatialStride`** — keep every Nth grid point in lat/lon. `2` is the
  default; raising it trades resolution for speed (`8` turns a ~350MB payload
  into ~22MB, which is handy when you're testing something else).

All three are read once at startup, so changing one means restarting.

### Seeing what you have

`catalog_data.py` inventories `data/` so you know what to put in
`wind_source.json`:

```bash
./.venv/bin/python catalog_data.py            # writes data/catalog.json
./.venv/bin/python catalog_data.py --print    # look without writing
```

It opens every `.nc`/`.grib` under `data/` (recursively — `.zip` downloads
extract into a subdirectory) and reports, per file: time steps and how they
group into contiguous spans, pressure levels, grid geometry, an estimated
payload size, and whether the file is usable as a wind source at all. It reads
**metadata only** — never the wind arrays — so cataloging an 8GB file takes
under a second.

```
OK 67e68497a30d3b5946ac20a319b63fd4.nc  (0.79 GB)
      time:    1 step(s); 1978-06-09T03:00:00 -> 1978-06-09T03:00:00 (1 @ single)
      levels:  37 (1000 -> 1 hPa)
      grid:    721x1440 @ 0.25deg, lat 90->-90, lon 0->359.75
      payload: stride 2 -> 361x720, ~352.2 MB JSON / ~73.4 MB f32 per time step

To serve one of these, set "file" in wind_source.json:
    "file": "67e68497a30d3b5946ac20a319b63fd4.nc"
```

Files it can't use are listed too, with the reason — an ERA5 single-level
download, for instance, reports `not a wind source: no pressure-level dim`.

It's an inspection tool only — the server doesn't read `catalog.json`.

**Which file is actually being served?** Ask the server. It's cheap, and never
touches the grids:

```bash
curl -s http://127.0.0.1:8000/api/wind-levels/source | python3 -m json.tool
```

### When something is wrong

The server does **not** refuse to start if it can't find data. It serves an
analytic mid-latitude jet instead (westerlies peaking near 225hPa around
45°N/45°S), prints a loud banner explaining what went wrong, and marks every
response:

```json
"source": {"synthetic": true, "problem": "\"file\" is not set in wind_source.json. ..."}
```

That's deliberate. `sim-server` fetches this endpoint exactly once at startup
and falls back to `WindField::zero()` on any error with a single `warn!` line,
so a configuration mistake otherwise surfaces as "the whole simulation runs,
balloons just never move" — a symptom that looks nothing like its cause.
Synthetic wind keeps the app usable and carries the explanation with the data.

### Performance

`/api/wind-levels` is built once at startup and served from cache (both the
dict and its pre-encoded JSON bytes) — see
`docs/investigations/WIND_TRANSFER_PERF.md` for the full breakdown. One thing
worth knowing if you're looking at that startup delay or at the code: the
response is also pre-gzipped at startup, but **nothing currently asks for
it**. `sim-server`'s `reqwest` client is built without the `gzip` Cargo
feature (`sim-server/Cargo.toml`), so it never sends `Accept-Encoding: gzip`
and always gets the larger, uncompressed response. The gzip path only fires
for a client that explicitly requests it (e.g. `curl -H "Accept-Encoding:
gzip"` or a browser). It's kept at a low `compresslevel` (1, not zlib's
default 6) precisely because it's currently unused insurance, not a real
request path worth spending startup time optimizing the compression ratio
for.

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

Then request **a single hour snapshot** — one timestamp, all 37 pressure
levels, global. That's what the server serves, so there's no reason to download
more to get started:

```python
import cdsapi

cdsapi.Client().retrieve(
    "reanalysis-era5-pressure-levels",
    {
        "product_type": ["reanalysis"],
        "variable": [
            # The two the server actually reads:
            "u_component_of_wind",
            "v_component_of_wind",
            # TODO(roy): additional variables to be specified.
            # Anything extra is ignored by wind_backend.py but costs download
            # size and disk — a single hour of u+v alone is much smaller than
            # the ~850MB you get by asking for the full instant-field set.
        ],
        "pressure_level": [
            "1", "2", "3", "5", "7", "10", "20", "30", "50", "70", "100",
            "125", "150", "175", "200", "225", "250", "300", "350", "400",
            "450", "500", "550", "600", "650", "700", "750", "775", "800",
            "825", "850", "875", "900", "925", "950", "975", "1000",
        ],
        "year": ["2026"],
        "month": ["06"],
        "day": ["09"],
        "time": ["12:00"],          # one hour — this is the snapshot
        # No "area" key: omitting it gives the global 721x1440 0.25deg grid.
        "data_format": "netcdf",
        "download_format": "unarchived",
    },
    "data/era5-pl-2026-06-09-1200.nc",
)
```

Then point the server at it and start:

```bash
./.venv/bin/python catalog_data.py     # confirm it looks right
# edit wind_source.json: "file": "era5-pl-2026-06-09-1200.nc"
./run.sh
```

Two things to watch:

- **The CDS API changes its key spellings occasionally** (`data_format` was
  `format`; `download_format` is newer still). If the request errors on a
  keyword, copy the auto-generated snippet from the dataset page's Download
  tab — that's always current — and keep `data_format: netcdf`.
- **ERA5 before 1979** has historically lived in a separate "preliminary back
  extension" dataset rather than the main one. If you want a pre-1979 date, you
  may need a different collection id.

Want less data? Drop pressure levels you don't care about — the balloons fly in
roughly the 50–200hPa band — or add an `area: [north, west, south, east]` key
to fetch a region instead of the globe.

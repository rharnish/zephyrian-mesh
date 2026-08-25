# Weather backend: direction & near-term plan

Notes from a design discussion about evolving `weather-data-server/` (the Python/FastAPI
wind backend) and how the frontend and `sim-server` consume it. Captures the reasoning so
we can pick it back up; the concrete near-term task is at the bottom.

## Where the wind data comes from today

The Python backend serves one static ERA5 snapshot — a single hour from June 1978,
downloaded from the [Copernicus CDS](https://cds.climate.copernicus.eu/) — over
`GET /api/wind-levels` (`weather-data-server/wind_backend.py`). That payload is the whole
global grid, all pressure levels, JSON-encoded nested float arrays: **146.2MB, ~0.04s to
serve** after the striding and compression work in
`docs/investigations/WIND_TRANSFER_PERF.md`. (The ~356MB / ~55s figures this section was
originally written against are that document's *before*, not its current state.)

> **Status: the duplicate-fetch problem described below has since been fixed.** It is kept
> in the present tense here because the rest of the plan builds on it; see
> "Near-term task — DONE" below for what actually shipped. Today the browser's wind fetch
> goes to `sim-server` (`WIND_API_URL` in `src/config.js`), not to Python, and sim-server
> loads from an on-disk cache rather than refetching per run.

At the time of writing, two independent clients fetched that same payload:

- **sim-server** (`load_wind_field(wind_from_args())` in `sim-server/src/main.rs`) — once at
  startup, into a Rust `WindField`. **This is the copy that actually advects balloons.** The
  startup path branches on a `--wind`/`ZM_WIND` flag (`wind_from_args()`) into a zero field,
  a cached snapshot via the `wind_cache` module, or (the `Auto` default) a live fetch from
  Python via `fetch_wind_field()` with the result cached for next time.
- **browser** (the `WindField.fetchFromBackend` call in `src/main.js`) — the *same* payload
  again, used **only** for the optional wind-vector-arrow visualization. Balloon physics is
  server-owned; the client's copy is purely decorative (see the comment above that call).

So the payload was transferred and parsed **twice**, by two clients, for two reasons.
The browser already talked to sim-server for everything else (towers, balloon count, horizon
coeff, the WebSocket snapshot stream) — wind was the one thing it still fetched straight from
Python. That was **historical, not principled**: the frontend owned wind first
(the header comment on `sim-server/src/wind_field.rs` records the port from
`src/windField.js`), sim-server copied the logic into Rust later, and the browser's fetch was
never re-pointed — until it was, below.

## Where we want it to go

### 1. Python is the "database" seam

Keep Python for now, but treat it as **a stand-in for a future data store**, not just where
the code happens to live. Its job becomes: *given a query (a time, a region, a set of
variables), return the matching raw data.* That's a database's job. Today it's backed by a
NetCDF file; later it could be Postgres, a tile/object store, or a live weather API — and
sim-server shouldn't have to change, because it only speaks the query protocol, not NetCDF.

Implication: the current `/api/wind-levels` (returns *everything*) is the one endpoint that
*doesn't* look like a database. A database-shaped interface wants parameters — `time=`,
`bbox=`, `levels=`, `vars=`. The moment Python accepts those, the 356MB problem reframes from
"transport is slow" to "the query is too coarse," and sim-server pulls only what it needs.

Keep the contract honest: sim-server should not leak NetCDF/file-snapshot assumptions across
the wire. Query-in / data-out keeps the "swap Python for a real DB later" story true.

### 2. sim-server becomes the sole client of the weather backend

Make the browser fetch wind from **sim-server**, not Python. Benefits:

- **Single ingestion point.** Only sim-server touches the weather backend. Python has exactly
  one consumer — easier to cache, evolve, and reason about.
- **sim-server can serve the browser a *cheaper* form.** It already holds the parsed grid in
  memory. For the arrows it can hand over a downsampled / single-level / binary slice instead
  of the full firehose. Because the browser's copy is only decorative, it can be reduced
  aggressively without touching sim physics.
- **Natural home for time interpolation (below).** If sim-server owns the working set and does
  the interpolation, both the physics and the browser arrows read one in-sync wind state —
  they can never disagree about "what the wind is right now."

### 3. Multi-timepoint data + time interpolation (later)

Desired features that motivated this: serve many timepoints (a day/week of hourly ERA5
instead of one snapshot), interpolate between two consecutive snapshots for continuously
variable wind, and add extra variables (sea-level pressure, temperature, …) for a richer sim.

These all converge on the architecture above: sim-server owns the multi-snapshot working set,
does the time bracketing + lerp, and serves both physics and browser from it. Python serves
individual parameterized slices on request.

The blocking prerequisite is still the payload: one snapshot is already 356MB/55s, so N
timepoints served the naive way is tens of GB. Fixing this = the parameterized query interface
(§1) + a cheaper encoding (binary/typed arrays, higher stride, fewer levels, gzip). See
`docs/investigations/WIND_TRANSFER_PERF.md` for the encoding options.

### Open fork (deferred)

Where NetCDF parsing + interpolation ultimately lives:

- **(A) Python stays a thin time-slice server** — sim-server requests specific times/regions,
  Python reads NetCDF and returns slices. *This is the chosen near-term direction* (Python as
  the database stand-in).
- **(B) Rust reads NetCDF directly** — sim-server reads files itself and Python goes away.
  Not now.

## Near-term task — DONE (commit 236f1e0, 2026-07-25)

**Re-pointed the browser's wind fetch from Python to sim-server.** First concrete step toward
the target architecture; removes the duplicate 356MB fetch and establishes sim-server as the
wind gateway.

- sim-server serves `GET /api/wind-levels` from the wind field it already holds in memory,
  shared as an `Arc<WindField>` between the sim task and the HTTP layer (no second copy).
  Serialized in the same shape the browser's `WindField` already deserializes, so the frontend
  change was just a URL swap.
- `src/config.js`'s `WIND_API_URL` now points at sim-server (8080) instead of Python (8000);
  `src/main.js` is otherwise unchanged.
- Python keeps serving sim-server, unchanged.
- Side effect Roy observed: frontend wind load is noticeably faster — serde_json from memory
  beats FastAPI re-encoding the nested lists per request.

Not yet done (deferred): serving the browser a *cheaper* (downsampled/binary) arrows-only slice
rather than the full field, and the parameterized query interface for §1 below.

# Zephyrian Mesh

A simulator for a constellation of high-altitude balloons that have to get
their telemetry to the ground without being told how.

Balloons drift on real ERA5 wind fields over a Cesium globe, forming and
losing line-of-sight radio links as they move. Each one generates telemetry
bundles that reach a ground tower only by being carried and relayed through
whatever neighbours happen to be in range.

## The constraint

No balloon is handed a picture of the network. A balloon learns who it can
reach from beacons it actually received, and forwards bundles toward the
tower it *believes* it has a route to — a belief that is often stale, and
sometimes wrong. Everything interesting lives in the gap between that belief
and the true connectivity graph, which the simulator computes separately so
the two can be compared.

Explored in [`docs/design/MESH_COMMS_DESIGN.md`](docs/design/MESH_COMMS_DESIGN.md); the
belief-vs-truth divergence is plotted in
[`experiments/protocol-results/`](experiments/protocol-results/).

## Layout

| Piece | What it does |
| --- | --- |
| `sim-server/` | Rust. Physics, link detection, and the pluggable comms protocols — the simulation's source of truth. |
| `src/` | Browser frontend. Renders whatever `sim-server` broadcasts over a WebSocket; holds no simulation state. |
| `weather-data-server/` | Python. Serves ERA5 wind fields to `sim-server`. |
| `experiments/` | Offline sweep binaries and their results. |
| `docs/` | Written-up investigations and protocol diagrams. |

**New to the comms side? Start with
[`docs/design/A-BUNDLES-LIFE.md`](docs/design/A-BUNDLES-LIFE.md)** — one telemetry
record followed from measurement to acknowledgement, with each design decision
named where it bites. It is the on-ramp to everything below.

Then the design notes:
[`docs/design/MESH_COMMS_DESIGN.md`](docs/design/MESH_COMMS_DESIGN.md) (the comms protocol),
[`docs/design/PROTOCOL_REFERENCE.md`](docs/design/PROTOCOL_REFERENCE.md) (all four protocols, diagrammed),
[`docs/design/TIMING_MODEL.md`](docs/design/TIMING_MODEL.md) (every clock in the system, in seconds),
and [`docs/design/BALLOON_PHYSICS_VISION.md`](docs/design/BALLOON_PHYSICS_VISION.md) (where the
buoyancy model is headed).

## User-specific setup

A few things are personal to your machine and deliberately not committed —
each has a template checked into git that you copy and fill in yourself:

| What | Template → your copy | Why it's not just committed |
| --- | --- | --- |
| Cesium Ion access token | `src/user-config-template.json` → `src/user-config.json` | It's a secret credential. |
| Which ERA5 file to serve | `weather-data-server/wind_source_template.json` → `weather-data-server/wind_source.json` | It names *your* downloaded file by its opaque Copernicus hash — nobody else has that exact file. |

Both real files are gitignored; only the `-template` versions are tracked.
Without them: the frontend can't load Cesium's globe imagery/terrain (no
token), and `wind_backend.py` serves a synthetic analytic jet stream instead
of real reanalysis wind (no data file configured) — both fail soft with an
explanatory message rather than crashing.

One more requirement that lives outside the repo entirely: acquiring ERA5
data in the first place needs a Copernicus Climate Data Store API key in
`~/.cdsapirc` — see
[`weather-data-server/README.md`](weather-data-server/README.md#acquiring-your-own-data)
for how to get one. You don't need this at all if you're fine with synthetic
wind.

## How to build and run

Set up your Cesium Ion token first — see
[User-specific setup](#user-specific-setup) above.

The app needs three processes running together: the wind data backend, the
Rust simulation server, and the Vite frontend.

```bash
npm install   # first time only
./run-all.sh
```

`run-all.sh` starts all three in order — waiting for each to be ready
before starting the next, skipping any that's already running — and stops
them together on Ctrl-C. The other two logs go to a temp dir (it prints
where). That's the whole quick start; the rest of this section is the
manual, three-terminal version, useful if you want to see each server's
output live or start just one of them.

```
┌──────────────────┐  /api/wind-levels/source  ┌──────────────┐   WS /ws    ┌────────────────┐
│  wind_backend.py │ ────────────────────────► │  sim-server  │ ──────────► │  browser       │
│  port 8000       │  then the grid itself,    │  port 8080   │ (snapshots) │  localhost:5173│
│                  │  only on a cache miss     │              │             │                │
└──────────────────┘                           └──────────────┘             └────────────────┘
```

### 1. Wind data — `wind_backend.py` (Python, port 8000)

```bash
cd weather-data-server
./run.sh
```

Serves the static ERA5 wind grid as JSON. Only `sim-server` fetches from it,
and only on a wind-cache miss — the browser's wind-vector-arrow overlay gets
its copy from `sim-server` instead (`WIND_API_URL` in `src/config.js`). With a
warm cache you can skip this step entirely; `run-all.sh` does.

`run.sh` uses a minimal venv local to this directory
(`weather-data-server/.venv`), creating it and installing
`requirements.txt` automatically on first run.

Sanity check it's up:

```bash
curl http://127.0.0.1:8000/api/wind-levels/meta
```

### 2. Simulation — `sim-server` (Rust, port 8080)

```bash
cd sim-server
cargo run --release
```

First build takes a minute or so; after that it's fast (cached). Owns
balloon/tower state, physics, and radio-link detection — see
[`sim-server/README.md`](sim-server/README.md) for how it fits together.
Wind is loaded from an on-disk cache when one exists, and fetched from
`wind_backend.py` only on a miss (then cached). So the ~46s the backend
spends building its ~146MB grid is paid once rather than every run, and
`run-all.sh` doesn't start the Python backend at all once the cache is warm. Choose a field with `--wind` — `auto` (default)
means cache-then-fetch, `none` means zero wind, and anything else names a
cached field by its observation time:

```bash
cd sim-server
cargo run --release --bin wind_cache -- steps   # what the .nc holds
cargo run --release --bin wind_cache -- fetch   # cache it (backend must be up)
cargo run --release --bin wind_cache -- list
```

With neither cache nor backend, `sim-server` logs a warning and falls back
to zero wind rather than failing to start.

Sanity check it's up (returns the wind-level metadata as JSON):

```bash
curl -s http://127.0.0.1:8080/api/wind-levels | head -c 200
```

`/ws` is a WebSocket upgrade endpoint, so a plain `curl` against it gets a
400/426 rather than snapshot JSON — use a WebSocket client, or just watch
`sim-server`'s own startup log line.

### 3. Frontend — Vite dev server (port 5173)

```bash
npm run dev
```

Opens at `http://localhost:5173/`. Globe, balloons, towers, and radio
links typically all appear within about 10 seconds, dominated by
Cesium's terrain load — sim-server seeds balloons/towers fresh on each of
its own restarts, not on frontend reloads, so reloading the browser just
reconnects to whatever state sim-server already has. The "Wind vectors"
toggle stays disabled a bit longer (shows "Loading wind data..."): it
fetches the full wind grid from `sim-server` in the background (which
serves it from whatever field it loaded) — nothing else waits on that
fetch.

### Stopping everything

Ctrl-C in each terminal, or:

```bash
# Find what is actually listening, then kill by PID. Safer than `pkill -f`,
# which will also match your own shell/editor if the pattern appears in its
# command line.
ss -ltnp 'sport = :8080 or sport = :8000 or sport = :5173'
kill <pid>            # one per port, from the output above
```

### Common issues

- **Globe loads but no balloons/towers/links ever appear**: check the
  browser console for `sim-server WebSocket error` — `sim-server` isn't
  running or isn't reachable at `SIM_SERVER_WS_URL` (`src/config.js`).
- **Balloons appear but never move / links never form**: `sim-server`
  fell back to zero wind — check its terminal for the
  `failed to fetch wind field ... using zero wind` warning, meaning
  `wind_backend.py` wasn't up when `sim-server` started. Restart
  `sim-server` after confirming `wind_backend.py` is reachable.
- **Port already in use**: another instance is likely still running from
  a previous session — `pgrep -af "sim-server|uvicorn|vite"` to find it.
- **Testing without opening a real browser**: see
  [`.claude/skills/run-cesium-app/SKILL.md`](.claude/skills/run-cesium-app/SKILL.md)
  for a headless Playwright driver (screenshots, JS eval, console capture).

## Verifying radio-link detection

Link detection (spatial grid + radio-range checks) now runs in `sim-server`,
not the browser — the frontend just renders whatever edges it broadcasts.
The correctness check that used to be manual panel buttons
("Verify links now" / debug transient-edge sampling) is now automated:

```bash
cd sim-server && cargo test
```

runs the grid algorithm against an O(n²) brute-force oracle over random
balloon fields (`grid_matches_brute_force_ground_truth` in
`sim-server/src/link_detection.rs`) and asserts they match exactly.

For headless/automated verification of the frontend itself (e.g. from an
agent), see the `.claude/skills/run-cesium-app` skill, which drives the app
in headless Chromium.
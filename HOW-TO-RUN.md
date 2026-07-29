# Running cesium-app

## Quick start

```bash
./run-all.sh
```

Starts `wind_backend.py`, then `sim-server`, then the frontend, in order —
waiting for each to be ready before starting the next, skipping any that's
already running (port already open). The frontend runs in the foreground;
Ctrl-C stops all three together. The other two logs go to a temp dir
(`run-all.sh` prints where).

The rest of this doc is the manual, three-terminal version of the same
sequence — useful if you want to see each server's output live, or start
just one of them.

Three processes, started in this order. Each one is a separate terminal
(or background job) that keeps running while you use the app.

```
┌──────────────────┐   GET /api/wind-levels   ┌──────────────┐   WS /ws   ┌─────────────┐
│  wind_backend.py   │ ───────────────────────► │  sim-server    │ ─────────► │  browser      │
│  port 8000          │      (once, startup)      │  port 8080      │  (snapshots) │  localhost:5173│
└──────────────────┘                            └──────────────┘            └─────────────┘
```

## 1. Wind data — `wind_backend.py` (Python, port 8000)

```bash
cd weather-data-server
./run.sh
```

Serves the static ERA5 wind grid as JSON. Start this first — both
`sim-server` and the browser's wind-vector-arrow overlay fetch from it.

`run.sh` uses a minimal venv local to this directory (`weather-data-server/.venv`),
not the `reginald` conda env — it creates the venv and installs
`requirements.txt` automatically on first run. If you still have `reginald`
set up and prefer it: `conda activate reginald && uvicorn wind_backend:app --reload`
works the same way, since both just run the same `wind_backend.py`.

Sanity check it's up:

```bash
curl http://127.0.0.1:8000/api/wind-levels/meta
```

## 2. Simulation — `sim-server` (Rust, port 8080)

```bash
cd sim-server
cargo run --release
```

First build takes a minute or so; after that `cargo run --release` is fast
(cached). Owns balloon/tower state, physics, and radio-link detection —
see [`sim-server/README.md`](sim-server/README.md) for how it fits together.

If `wind_backend.py` isn't reachable yet, `sim-server` logs a warning and
falls back to zero wind rather than failing to start — start
`wind_backend.py` first to avoid that, or just restart `sim-server` once it
is up.

Sanity check it's up (should hang open, printing snapshot JSON — Ctrl-C to
stop):

```bash
curl -N http://127.0.0.1:8080/ws  # or use a real WS client; this is just a reachability check
```

## 3. Frontend — Vite dev server (port 5173)

```bash
npm install   # first time only
npm run dev
```

Opens at `http://localhost:5173/`. Globe, balloons, towers, and radio links
typically all appear within about 10 seconds, dominated by Cesium's terrain
load — sim-server seeds balloons/towers fresh on each of its own restarts,
not on frontend reloads, so reloading the browser just reconnects to
whatever state sim-server already has. The "Wind vectors" panel control
stays disabled a bit longer while it fetches the (large, slow) full wind
grid in the background — nothing else waits on that fetch.

## Stopping everything

Ctrl-C in each terminal, or:

```bash
pkill -f "target/release/sim-server"
pkill -f "uvicorn wind_backend"
pkill -f "vite --port"
```

## Common issues

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

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

Explored in [`MESH_COMMS_DESIGN.md`](MESH_COMMS_DESIGN.md); the
belief-vs-truth divergence is plotted in
[`experiments/protocol-results/`](experiments/protocol-results/).

## Layout

| Piece | What it does |
| --- | --- |
| `sim-server/` | Rust. Physics, link detection, the beacon and bundle protocols — the simulation's source of truth. |
| `src/` | Browser frontend. Renders whatever `sim-server` broadcasts over a WebSocket; holds no simulation state. |
| `weather-data-server/` | Python. Serves ERA5 wind fields to `sim-server`. |
| `experiments/` | Offline sweep binaries and their results. |
| `docs/` | Written-up investigations and protocol diagrams. |

Design notes worth reading first:
[`MESH_COMMS_DESIGN.md`](MESH_COMMS_DESIGN.md) (the comms protocol),
[`TIMING_MODEL.md`](TIMING_MODEL.md) (every clock in the system, in seconds),
and [`BALLOON_PHYSICS_VISION.md`](BALLOON_PHYSICS_VISION.md) (where the
buoyancy model is headed).

## How to build and run

copy `src/user-config-template.json` to `src/user-config.json` and add your
own Cesium Ion access token, etc. (`src/user-config.json` is gitignored —
never commit your real token).

The app needs three processes running together (wind data backend, the
Rust simulation server, and the Vite frontend) — see
[`RUNNING.md`](RUNNING.md) for exact commands, startup order, and
troubleshooting. Short version:

```bash
npm install   # first time only
./run-all.sh
```

`run-all.sh` starts all three in order and stops them together on Ctrl-C.
See `RUNNING.md` for the manual three-terminal version, if you'd rather
watch each server's output separately.

Opens a globe with animated balloons and towers — everything (globe,
balloons, towers, radio links) typically appears within about 10 seconds,
dominated by Cesium's terrain load. The "Wind vectors" toggle stays
disabled a bit longer (shows "Loading wind data..."): it fetches the full
wind grid from `wind_backend.py` in the background, which is a large,
slow payload (see `weather-data-server/README.md`) — nothing else waits
on it.

Balloon/tower simulation state now lives in `sim-server` (Rust) — see
[`sim-server/README.md`](sim-server/README.md) for how it fits with the
Cesium frontend. Wind data is fetched from `WIND_API_URL` in
`src/config.js` (`http://127.0.0.1:8000/api/wind-levels` by default). If
that backend isn't running, `sim-server` falls back to zero wind rather
than failing.

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
## Zepheryian Dirigble Network

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
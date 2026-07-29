# sim-server — how this fits with the Cesium app

## What this is

`sim-server` is a standalone Rust binary implementing **Plan B** from
[`docs/history/RUST_SIM_PLAN.md`](../docs/history/RUST_SIM_PLAN.md): the balloon/tower simulation
(wind advection, altitude control, radio-link detection) runs here instead
of in the browser. It's a separate long-running process — not a library the
browser loads directly (that would be Plan A, the WASM approach; not built).

**Nothing in `weather-data-server/wind_backend.py` changes.** `sim-server` is just
another *client* of it, the same way the browser used to be.

## The three processes, today

```
┌──────────────────────┐      GET /api/wind-levels      ┌──────────────────┐
│  wind_backend.py      │ ◄──────────────────────────── │   sim-server      │
│  (FastAPI, port 8000)  │        (once, at startup)      │  (Rust, port 8080)│
│  serves static ERA5    │                                 │  owns the World:  │
│  wind grid as JSON     │                                 │  balloons, towers,│
└──────────────────────┘                                 │  wind, physics loop│
                                                            └────────┬─────────┘
                                                                     │ WS /ws
                                                                     │ JSON snapshots
                                                                     │ every tick
                                                            ┌────────▼─────────┐
                                                            │  cesium-app (browser)│
                                                            │  Cesium rendering    │
                                                            │  only — no physics   │
                                                            └──────────────────────┘
```

1. **`weather-data-server/wind_backend.py`** (unchanged) parses the static
   ERA5 NetCDF file and serves it as JSON. Still runs via
   `uvicorn wind_backend:app --reload` in the `reginald` conda env, same as
   before — just from its renamed directory (was `WeatherData/`).
2. **`sim-server`** fetches that JSON once at startup, then owns the live
   simulation state (`World` in `src/sim.rs`): balloon positions, wind
   advection, altitude control, and radio-link/cluster detection. It ticks
   on its own clock (20Hz wall-clock, each tick advancing `TICK_DT_SECONDS *
   TIME_SCALE` simulated seconds — same constants as `config.js` had) and
   broadcasts a JSON snapshot to every connected client over a WebSocket.
3. **`cesium-app`** (the Vite/Cesium frontend) is a *pure renderer*:
   `src/main.js` opens a WebSocket to `sim-server` and reconciles Cesium
   entities against whatever snapshot arrives — no local physics or
   link-detection state. User actions (add/remove tower, change balloon
   count, change horizon coefficient) go out as REST calls to `sim-server`
   (`POST /api/towers`, `DELETE /api/towers/:id`,
   `POST /api/balloons/count`, `POST /api/horizon-coeff`) rather than
   mutating anything locally. This wiring is done (see `docs/history/RUST_SIM_PLAN.md`,
   section B6, and the "Client wiring" section below for specifics).

The panel's old "Verify links now" / "Debug: catch missed brief passes"
buttons are gone — they checked client-side link-detection state that no
longer exists there. The equivalent check now lives in
`cargo test` (`grid_matches_brute_force_ground_truth`, server-side).

## Why the domain logic looks the way it does

Every module in `src/` is a deliberate line-for-line port of an existing JS
file, so the two stay easy to diff against each other and (for now) behave
identically. The four JS originals that had no other purpose left in the
live app once this port landed (`balloon.js`, `spatialGrid.js`,
`unionFind.js`, `linkDetection.js`) have since been archived to
`experiments/legacy-js/src/` — they're kept only for the JS connectivity
sweep, see `experiments/legacy-js/README.md`:

| Rust file | Ported from | What it does |
|---|---|---|
| `wind_field.rs` | `src/windField.js` | Bilinear interp per pressure level + altitude blending between levels |
| `balloon.rs` | `experiments/legacy-js/src/balloon.js` | Wind advection + target-altitude thermostat |
| `geo.rs` | `src/geo.js` | Radio horizon, great-circle distance, random sphere point |
| `spatial_grid.rs` | `experiments/legacy-js/src/spatialGrid.js` | Lon/lat cell bucketing for neighbor queries |
| `union_find.rs` | `experiments/legacy-js/src/unionFind.js` | Cluster detection (grounded vs. ungrounded) |
| `link_detection.rs` | `experiments/legacy-js/src/linkDetection.js` | Grid-based edge finder + O(n²) brute-force oracle used in tests |
| `tower.rs` | `src/towerModel.js` | Plain tower data (no Cesium entity — that stays client-side) |
| `sim.rs` | `src/main.js`'s `tick()` | Owns `World`, runs the tick loop, throttles link recompute the same way (`LINK_UPDATE_EVERY_N_TICKS`) |
| `config.rs` | `src/config.js` | Hand-mirrored constants — **keep these in sync manually** until one side is deleted |

Cesium-specific code (entities, `PolylineCollection`, the range-gradient
canvas texture in `tower.js`) has **no Rust equivalent** and never will —
that's rendering, and rendering stays in the browser regardless of which
plan wins.

## Client wiring

`src/main.js` maintains two reconciliation maps (`balloonEntities`,
`towerById`) keyed by the server's ids, updated on every WebSocket message:
add an entity for a new id, update position for a known one, remove an
entity whose id no longer appears in the snapshot. `syncLinks()` does the
same for radio-link polylines, keyed by the edge's `pairKey`.

Server routes the frontend calls:

| Route | Frontend trigger |
|---|---|
| `GET /ws` | opened once on load; drives all rendering |
| `POST /api/towers` | click on empty globe |
| `DELETE /api/towers/:id` | click on an existing tower |
| `POST /api/balloons/count` | "Apply" button next to the balloon-count input |
| `POST /api/horizon-coeff` | horizon-coefficient slider, on release (`change`, not `input` — avoids flooding the server with a request per drag pixel) |

The wind field fetched directly by the browser (`WindField.fetchFromBackend`
in `src/main.js`) is now used **only** for the wind-vector-arrow
visualization — `sim-server` has its own independently-fetched copy for
actual balloon advection.

## A bug this port caught

Writing `link_detection.rs`'s test (`grid_matches_brute_force_ground_truth`,
which checks the grid algorithm against an O(n²) brute-force oracle over
random balloon fields) surfaced a real gap: near either pole, two nodes on
opposite sides can be close in great-circle distance while differing in
longitude by up to 180°. The grid's search window didn't account for
wrapping *over* the pole, so it could miss a real in-range pair. Fixed in
both `spatial_grid.rs` and the original (now archived)
`experiments/legacy-js/src/spatialGrid.js` (same fix, kept in sync) — see
the `pole_wraparound_edges_are_not_missed` test and the comment in
`neighbors()` in either file.

## Running it

See [`HOW-TO-RUN.md`](../HOW-TO-RUN.md) for the full three-process startup
sequence (wind backend, sim-server, frontend) and troubleshooting. Short
version, just this server:

```bash
cd sim-server
cargo run --release
```

`sim-server` logs whether it loaded real wind data or fell back to zero
wind, then listens on `ws://127.0.0.1:8080/ws`.

```bash
cd sim-server
cargo test
```

runs the ported unit tests, including the grid-vs-brute-force correctness
check.

## Repo layout

`sim-server/` is a top-level peer of `src/` and `weather-data-server/` within the
`cesium-app` repo — a sibling directory, not nested under an extra `rust/`
wrapper. It doesn't depend on being inside `cesium-app/`; it only talks to
`wind_backend.py` over HTTP and to the browser over WebSocket. If it ever
needs independent deploy/versioning/CI, splitting it into its own repo is a
plain `git filter-repo`/subtree-split away — no code changes required.

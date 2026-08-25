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
┌────────────────────────┐   GET /api/wind-levels/source  ┌──────────────────────┐
│  wind_backend.py       │ ◄───────────────────────────── │  sim-server          │
│  (FastAPI, port 8000)  │   then the grid only on a      │  (Rust, port 8080)   │
│  serves static ERA5    │   wind-cache miss, at startup  │  owns the World:     │
│  wind grid as JSON     │                                │  balloons, towers,   │
└────────────────────────┘                                │  wind, physics loop  │
                                                          └──────────┬───────────┘
                                                                     │ WS /ws
                                                                     │ JSON snapshots
                                                                     │ every tick
                                                          ┌──────────▼───────────┐
                                                          │  browser frontend    │
                                                          │  Cesium rendering    │
                                                          │  only — no physics   │
                                                          └──────────────────────┘
```

1. **`weather-data-server/wind_backend.py`** (unchanged) parses the static
   ERA5 NetCDF file and serves it as JSON. Still started via `./run.sh`.
2. **`sim-server`** resolves a wind field at startup — asking the backend
   only which field it serves, then loading it from the on-disk wind cache
   and fetching the full grid only on a cache miss (falling back to the
   newest cached field if the backend is unreachable, and to zero wind only
   if the cache is empty too) — then owns the live
   simulation state (`World` in `src/sim.rs`): balloon positions, wind
   advection, altitude control, and radio-link/cluster detection. It ticks
   on its own clock (20Hz wall-clock, each tick advancing `TICK_DT_SECONDS *
   TIME_SCALE` simulated seconds — same constants as `config.js` had) and
   broadcasts a JSON snapshot to every connected client over a WebSocket.
3. **`src/`** (the Vite/Cesium frontend) is a *pure renderer*:
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
entity whose id no longer appears in the snapshot. `src/linkLayer.js` does
the same for radio-link polylines, keyed by the edge's `pairKey`.

Server routes the frontend calls:

| Route | Frontend trigger |
|---|---|
| `GET /ws` | opened once on load; drives all rendering |
| `POST /api/towers` | click on empty globe |
| `DELETE /api/towers/:id` | click on an existing tower |
| `POST /api/balloons/count` | "Apply" button next to the balloon-count input |
| `POST /api/horizon-coeff` | horizon-coefficient slider, on release (`change`, not `input` — avoids flooding the server with a request per drag pixel) |
| `POST /api/paused` | pause/resume control |
| `GET /api/wind-levels` | the browser's wind-vector overlay (served by `sim-server`, not the Python backend) |
| `GET /api/balloons/:id/comms` | inspector panel's per-balloon comms log |

The wind field fetched by the browser (`WindField.fetchFromBackend` in
`src/windField.js`, called from `src/main.js`) is used **only** for the
wind-vector-arrow visualization. It no longer goes to the Python backend:
`WIND_API_URL` in `src/config.js` points at `sim-server`, which serves the
copy it already holds. `sim-server` uses that same field for balloon
advection, so the two can no longer disagree.

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

See [`README.md`](../README.md#how-to-build-and-run) for the full
three-process startup sequence (wind backend, sim-server, frontend) and
troubleshooting. Short version, just this server:

```bash
cd sim-server
cargo run --release
```

`sim-server` logs whether it loaded real wind data or fell back to zero
wind, then listens on `ws://127.0.0.1:8080/ws`.

Environment variables, all optional and all overridden by the equivalent
command-line flag where one exists:

| Variable | Effect |
|---|---|
| `MESH_PROTOCOL` | Protocol spec to run, same syntax as `--protocol` (e.g. `dv-dtn:ack=digest`). Defaults to the shipped `dv-dtn`. |
| `ZM_WIND` | Wind field to load, same values as `--wind`: `auto`, `none`, or a cached field's observation time. |
| `ZM_WIND_CACHE` | Directory holding cached wind fields. Defaults to `sim-server/.wind-cache`. |
| `PREFER_NEARER` | `1` makes the `bundle_delivery` experiment binary route nearest-first instead of freshest-first (`Metric::NearestFirst`). Experiment binary only. |

```bash
cd sim-server
cargo test
```

runs the ported unit tests, including the grid-vs-brute-force correctness
check.

## Repo layout

`sim-server/` is a top-level peer of `src/` and `weather-data-server/` within the
`zephyrian-mesh` repo — a sibling directory, not nested under an extra `rust/`
wrapper. It doesn't depend on being inside that repo; it only talks to
`wind_backend.py` over HTTP and to the browser over WebSocket. If it ever
needs independent deploy/versioning/CI, splitting it into its own repo is a
plain `git filter-repo`/subtree-split away — no code changes required.

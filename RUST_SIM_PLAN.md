# Rust simulation core — two implementation plans

**Status: Plan B is built and running** — see
[`sim-server/README.md`](sim-server/README.md) for the current
architecture, how the pieces fit together, and how to run it. This
document is kept for reference (the reasoning behind the two options,
and Plan A's steps if that's ever picked up later) rather than as a
live task list.

Context (as it was before Plan B): physics used to live client-side in
`src/balloon.js` (wind advection + altitude thermostat) and
`src/linkDetection.js` / `src/spatialGrid.js` / `src/unionFind.js`
(radio-link detection between balloons and towers). It now lives in
`sim-server` (Rust); `src/main.js` is a thin renderer. Those four JS files
are no longer in `src/` — they've since been archived to
`experiments/legacy-js/src/`, kept only for the JS connectivity sweep (see
`experiments/legacy-js/README.md`); paths below are as they were pre-move.
`weather-data-server/wind_backend.py` (renamed from `WeatherData/`) only
serves static wind grid data — it did no simulation before this and still
doesn't; both the old client-side code and `sim-server` are/were just
clients of it.

Each plan is broken into small, mechanical steps with exact file paths and
function signatures so it can be executed by a model with less context, one
step at a time, verifying after each step.

Pick ONE plan to execute. Do not mix them.

---

## Plan A — WASM module (physics runs in-browser, replaces JS tick loop)

**Goal:** move the per-tick balloon motion and link-detection math into Rust,
compiled to WebAssembly, called from `src/main.js`. Cesium
rendering (entities, polylines, camera) stays in JS — only the number
crunching moves.

### A0. Prerequisites
- Install Rust toolchain + `wasm-pack` (`cargo install wasm-pack`) if not present.
- Confirm `rustc --version` and `wasm-pack --version` both run.

### A1. Scaffold the crate
- `cargo new --lib rust/sim-core` at the repo root (sibling to `cesium-app/`,
  or inside `cesium-app/rust/sim-core` — keep it inside `cesium-app/` so the
  build tooling is co-located).
- Add to `rust/sim-core/Cargo.toml`:
  ```toml
  [lib]
  crate-type = ["cdylib", "rlib"]

  [dependencies]
  wasm-bindgen = "0.2"
  ```

### A2. Port data structures (no wasm-bindgen yet — plain Rust, unit-testable)
File: `rust/sim-core/src/wind_field.rs`
- Port `src/windField.js` exactly:
  - `struct WindHeader { nx: usize, ny: usize, lo1: f64, la1: f64, lo2: f64, la2: f64, dx: f64, dy: f64 }`
  - `struct Level { pressure_hpa: f64, altitude_m: f64, u_data: Vec<Vec<f32>>, v_data: Vec<Vec<f32>> }`
  - `struct WindField { header: WindHeader, levels: Vec<Level> }`
  - `impl WindField { fn normalize_lon(&self, lon: f64) -> f64; fn sample_level(&self, level: &Level, lon: f64, lat: f64) -> (f32, f32); fn sample(&self, lon: f64, lat: f64, alt_m: f64) -> (f32, f32) }`
  - Copy the bilinear interpolation and level-bracketing logic line-for-line
    from `windField.js` — do not "improve" it in this step, just translate.
- Write `#[cfg(test)]` unit tests: hand-construct a tiny 2x2 WindField and
  assert `sample()` matches hand-computed values at the corners and center.

File: `rust/sim-core/src/balloon.rs`
- Port `src/balloon.js`:
  - `struct Balloon { id: u32, lon: f64, lat: f64, alt: f64, target_alt: f64 }`
  - Constants ported from `src/config.js`: `BALLOON_MIN_ALT`, `BALLOON_MAX_ALT`,
    `MAX_VERTICAL_RATE`, `VERTICAL_GAIN`, `TARGET_DRIFT_CHANCE_PER_TICK`,
    `TARGET_DRIFT_RANGE`.
  - `impl Balloon { fn step(&mut self, dt_seconds: f64, wind: &WindField, rng: &mut impl Rng) }`
    — port the horizontal-advection + longitude-wrap + vertical-controller
    logic exactly as in `balloon.js:27-55`. Use the `rand` crate for the
    `Math.random()` calls (add `rand = "0.8"` to Cargo.toml; for wasm target
    add `getrandom = { version = "0.2", features = ["js"] }` so `rand` works
    in-browser).
- Unit test: zero wind + target == current alt → position unchanged after
  `step()`. Nonzero wind → position moves in the expected direction.

File: `rust/sim-core/src/geo.rs`
- Port whatever `src/geo.js` exposes that `linkDetection.js` uses:
  `precomputeNode`, `inRadioRangePrecomputed`, `maxPossibleRangeKm`,
  `randomGlobalPosition`. Read `src/geo.js` first and mirror its exact math
  (this is the horizon/radio-range geometry — do not re-derive it, translate
  it).

File: `rust/sim-core/src/spatial_grid.rs` and `rust/sim-core/src/union_find.rs`
- Port `src/spatialGrid.js` and `src/unionFind.js` directly — these are small,
  simple data structures (grid-bucketed neighbor lookup; path-compressed
  union-find). Keep the same method names translated to snake_case so the
  diff against the JS is easy to eyeball.

File: `rust/sim-core/src/link_detection.rs`
- Port `src/linkDetection.js`'s `computeGridEdges` (the production path) and
  `bruteForceEdgeKeys` (ground truth). Keep both — the brute-force version is
  the correctness oracle used in step A4.
- `struct Edge { a_id: String, b_id: String, pos_a: (f64,f64,f64), pos_b: (f64,f64,f64) }`
  (positions here are lon/lat/alt; Cesium `Cartesian3` conversion stays in JS).

### A3. wasm-bindgen surface
File: `rust/sim-core/src/lib.rs`
- Expose a `#[wasm_bindgen] struct Sim` that owns `Vec<Balloon>`,
  `Vec<Tower>`, `WindField`, `SpatialGrid`, `UnionFind` — mirroring the state
  currently held in `main.js`'s closures.
- Methods (all `#[wasm_bindgen]`):
  - `Sim::new(wind_json: &str) -> Sim` — parse the same JSON shape
    `wind_backend.py`'s `/api/wind-levels` already returns (use `serde` +
    `serde_json`, add `serde = { version = "1", features = ["derive"] }`,
    `serde_json = "1"`).
  - `spawn_balloons(&mut self, n: u32)` — same random spawn as
    `main.js:90-105`.
  - `add_tower(&mut self, lon: f64, lat: f64, height_m: f64)`,
    `remove_tower(&mut self, index: usize)`.
  - `step(&mut self, dt_seconds: f64)` — advance all balloons one tick.
  - `balloon_positions(&self) -> Vec<f64>` — flat `[lon0,lat0,alt0,lon1,...]`
    for cheap transfer across the wasm boundary (avoid returning
    `Vec<JsValue>` per-balloon; a flat typed array is much faster).
  - `compute_edges(&mut self) -> String` — return JSON (via `serde_json`) of
    `[{aKey, bKey, ax,ay,az, bx,by,bz}]`; JS re-attaches Cesium positions from
    live entities as it does today, or you can pass positions through
    directly since Rust already has lon/lat/alt.
- Keep `bruteForceEdgeKeys` reachable too, e.g. `verify_edges(&mut self) -> String`,
  so the existing `verifyEdgesOnce` debug button in the panel keeps working.

### A4. Correctness gate before wiring into the render loop
- `cargo test` must pass for every module above.
- Add one integration test in `rust/sim-core/tests/`: spawn N random
  balloons + towers, run `compute_edges` (grid) and `verify_edges`
  (brute-force), assert identical edge sets — this is the Rust equivalent of
  `verifyEdgesOnce` in `linkDetection.js:84-106`, run at build time instead of
  by hand.

### A5. Build + wire into the frontend
- `wasm-pack build rust/sim-core --target web --out-dir ../../src/wasm-sim`
  (adjust paths so output lands under `cesium-app/src/wasm-sim/`).
- Add an npm script in `package.json`: `"build:wasm": "wasm-pack build rust/sim-core --target web --out-dir ../../src/wasm-sim"`.
- In `src/main.js`:
  - Replace the import block and state currently backing the tick loop
    (`balloons`, `grid`, `unionFind`, the `spawnBalloons` function, and the
    body of `tick()` lines 150-201) with:
    1. `import init, { Sim } from './wasm-sim/sim_core.js';` and `await init();` near the top of `initCesium()`.
    2. `const sim = Sim.new(JSON.stringify(windFieldJsonFetchedEarlier));` — fetch
       the raw wind JSON once (as today) and hand the raw JSON string to Rust
       instead of constructing a JS `WindField`.
    3. `sim.spawn_balloons(params.numBalloons)` in place of the JS
       `spawnBalloons` body; keep the JS-side `Cesium.Entity` creation/removal
       (rendering only) driven off `sim.balloon_positions()`.
    4. In `tick()`: call `sim.step(dt)`, read back `sim.balloon_positions()`,
       update each entity's position from the flat array. Every
       `LINK_UPDATE_EVERY_N_TICKS` ticks, call `sim.compute_edges()`, parse the
       JSON, and feed it into the existing `syncLinks()` unchanged (that
       function only needs `{pairKey, posA, posB, color}` — same shape you get
       today, so no changes to `syncLinks()` itself).
  - Everything below `syncLinks` (union-find coloring, panel wiring, wind
    vector arrows) stays in JS as-is; union-find over the returned edges can
    stay in JS too (it's cheap) or move to Rust if you already ported it in
    A2 — either is fine, don't over-engineer this step.

### A6. Validate and clean up
- Run the app, bump `params.numBalloons` to 2000–5000 via the panel, confirm
  frame time is better than the pre-Rust baseline (check devtools
  performance tab).
- Once satisfied, delete the now-dead JS physics code
  (`src/balloon.js`'s `step`, the JS `computeGridEdges`/`bruteForceEdgeKeys`
  bodies) — but only after A6's manual check passes, and only if nothing
  else still imports them (`grep -rn "computeGridEdges\|bruteForceEdgeKeys" src/`).

---

## Plan B — standalone Rust sim server (physics runs server-side, browser is a thin client)

**Goal:** a long-running Rust process owns the balloon/tower state and the
physics loop; the browser connects, receives state updates, and only
renders. This is the right shape if you want the simulation to keep running
independent of any open tab, or to have multiple viewers watch the same
live world.

**Decision needed before starting:** does every browser tab get its own
private simulation (matches today's UX — reload = fresh random balloons), or
is there one shared world all tabs observe? Plan below assumes **one shared
world** (simpler server, and the more interesting "server owns truth" case);
note where it'd differ for per-connection sims.

### B0. Prerequisites
- Rust toolchain installed (no wasm-pack needed here).

### B1. Scaffold the crate
- `cargo new sim-server` (binary crate) at the repo root.
- `Cargo.toml` dependencies: `tokio = { version = "1", features = ["full"] }`,
  `axum = "0.7"` (REST + WebSocket in one framework), `serde`, `serde_json`,
  `reqwest = { version = "0.12", features = ["json"] }` (to fetch wind data
  from the existing Python backend at startup), `tokio-stream`.

### B2. Reuse the domain logic from Plan A
- If Plan A was done first, move `rust/sim-core`'s non-wasm modules
  (`wind_field.rs`, `balloon.rs`, `geo.rs`, `spatial_grid.rs`, `union_find.rs`,
  `link_detection.rs`) into a shared library crate `rust/sim-core` with
  `crate-type = ["rlib"]` only (drop `cdylib`/wasm-bindgen dependency for this
  plan), and depend on it from both `sim-core`'s wasm build and
  `sim-server`.
- If starting fresh (Plan A not done), port the same modules directly per
  the instructions in Plan A's step A2 — the math is identical, only the
  eventual `#[wasm_bindgen]` wrapper differs.

### B3. Startup: fetch wind data
File: `sim-server/src/main.rs`
- On startup, `reqwest::get("http://127.0.0.1:8000/api/wind-levels")`
  (same `WIND_API_URL` as `src/config.js`), parse into the `WindField`
  struct from B2. Fail fast with a clear error if the Python backend isn't
  running (mirrors the `preload_dataset` fail-fast behavior in
  `wind_backend.py:119-128`).

### B4. Shared simulation state
File: `sim-server/src/sim.rs`
- `struct World { balloons: Vec<Balloon>, towers: Vec<Tower>, wind: WindField, grid: SpatialGrid, union_find: UnionFind }`
- Own it in a single background task (not behind a `Mutex` shared across
  request handlers) — a `tokio::task::spawn` loop that ticks on a
  `tokio::time::interval`, mutates `World` directly, then serializes a
  snapshot and sends it on a `tokio::sync::broadcast::Sender<String>`
  channel. This avoids lock contention and matches the existing JS model of
  "one authoritative tick loop."
- Tick body mirrors `main.js`'s `tick()` (lines 150-201): step every
  balloon, and every `LINK_UPDATE_EVERY_N_TICKS` ticks recompute edges +
  union-find grounded/ungrounded coloring, same constants as
  `src/config.js`.
- Snapshot JSON shape (serde-serialize this) — keep it identical to what
  `syncLinks()` already expects on the frontend so `src/main.js`'s rendering
  code barely changes:
  ```json
  {
    "balloons": [{"id": 0, "lon": ..., "lat": ..., "alt": ...}, ...],
    "edges": [{"pairKey": "b0|b1", "posA": [lon,lat,alt], "posB": [...], "grounded": true}]
  }
  ```

### B5. HTTP/WebSocket API
File: `sim-server/src/main.rs` (axum router)
- `GET /ws` — upgrades to WebSocket, subscribes to the broadcast channel from
  B4, forwards every snapshot to the client as a text frame.
- `POST /api/towers` `{lon, lat, heightM}` — pushes a tower into `World`
  (needs a command channel into the tick task, e.g.
  `tokio::sync::mpsc::Sender<Command>`, since the tick task owns `World`
  exclusively).
- `DELETE /api/towers/{index}` — same pattern, mirrors `main.js`'s
  click-to-remove-tower handler (lines 68-77).
- `POST /api/balloons/count` `{n}` — respawn command, mirrors the panel's
  "Apply" button (`main.js:325-330`).

### B6. Frontend changes
File: `src/main.js`
- Remove the entire local tick loop's physics/link portion (same lines as
  Plan A's A5 step 1, `150-201`), and the local `balloons`/`grid`/
  `unionFind`/`spawnBalloons` state — the server now owns all of that.
- Open a WebSocket to the Rust server's `/ws` on init; on each message,
  parse the JSON snapshot and:
  - Reconcile the `viewer.entities` balloon list against `snapshot.balloons`
    (add/remove/update positions) instead of stepping local `Balloon`
    objects.
  - Feed `snapshot.edges` directly into the existing `syncLinks()` — its
    input shape (`pairKey`, `posA`, `posB`, `color`) barely changes; compute
    `color` client-side from the `grounded` boolean the same way
    `main.js:186-190` does today.
- Panel's "Apply" (balloon count) button now does
  `fetch('http://.../api/balloons/count', {method:'POST', body: JSON.stringify({n})})`
  instead of calling local `spawnBalloons`. Tower add/remove clicks likewise
  become `POST`/`DELETE` calls instead of local array mutation.
- Add a `SIM_SERVER_URL` constant to `src/config.js` alongside `WIND_API_URL`.

### B7. Validate
- Run `wind_backend.py` (existing), then `cargo run --release` for
  `sim-server`, then the Vite dev server. Confirm balloons move and links
  render exactly as before.
- Open two browser tabs — both should show the same balloon positions and
  links (proof the server, not the browser, owns state), if you went with
  the shared-world design in B0.
- `cargo test` for `sim-core`'s ported modules (same tests as Plan A's A2/A4)
  should still pass unchanged, since the domain logic is shared.

---

## Which to pick

- **Plan A (WASM)** if the goal is "make today's single-tab simulator handle
  many more balloons smoothly" — smaller scope, no new deployment story, no
  server process to run/manage.
- **Plan B (server)** if the goal is "the simulation should exist
  independent of any browser tab" (e.g. multiple observers, or a persistent
  backend you can query/administer) — bigger scope, adds a process to run
  and a network protocol to version.

Both plans leave `wind_backend.py` untouched; only the balloon-motion and
link-detection code paths move to Rust.

# Architecture

Three processes, started together by `run-all.sh` (see [RUNNING.md](RUNNING.md)):

```mermaid
flowchart TB
    subgraph client["Browser Client — Vite + CesiumJS (src/)"]
        main["main.js<br/>orchestrator: init Cesium viewer,<br/>reconcile balloons/towers, wire UI"]
        config_js["config.js<br/>URLs & params"]
        windField_js["windField.js<br/>WindField model"]
        windVectors["windVectors.js<br/>wind arrow visualization"]
        tower_js["tower.js / towerModel.js<br/>tower rendering"]
        geo_js["geo.js<br/>geo math helpers"]

        main --> config_js
        main --> windField_js
        main --> windVectors
        main --> tower_js
        main --> geo_js
    end

    subgraph sim["sim-server — Rust / Axum (sim-server/src/)"]
        main_rs["main.rs<br/>owns the single World task,<br/>ws + REST routes"]
        sim_rs["sim.rs<br/>World: tick loop, Command handling,<br/>Snapshot broadcast"]
        balloon_rs["balloon.rs<br/>balloon state + advection"]
        tower_rs["tower.rs<br/>tower state"]
        wind_field_rs["wind_field.rs<br/>WindField (Rust copy)"]
        link_detection["link_detection.rs<br/>compute_grid_edges<br/>(+ brute-force oracle for tests)"]
        spatial_grid["spatial_grid.rs<br/>SpatialGrid"]
        union_find["union_find.rs<br/>UnionFind"]
        geo_rs["geo.rs<br/>horizon_km, precompute"]
        config_rs["config.rs<br/>tunables (tick rate,<br/>balloon count, ports...)"]

        main_rs --> sim_rs
        sim_rs --> balloon_rs
        sim_rs --> tower_rs
        sim_rs --> wind_field_rs
        sim_rs --> link_detection
        link_detection --> spatial_grid
        link_detection --> geo_rs
        sim_rs --> union_find
        sim_rs --> config_rs
    end

    subgraph wind["weather-data-server — Python / FastAPI"]
        wind_backend["wind_backend.py<br/>serves ERA5 pressure-level<br/>wind data via xarray"]
    end

    main -- "WebSocket /ws<br/>(Snapshot: balloons, towers, edges,<br/>paused, horizonRefractionCoeff)" --> main_rs
    main -- "REST: POST/DELETE /api/towers,<br/>POST /api/balloons/count,<br/>POST /api/horizon-coeff,<br/>POST /api/paused" --> main_rs
    main -- "GET /api/wind-levels<br/>(background, for arrow overlay)" --> main_rs
    main_rs -- "GET /api/wind-levels<br/>(startup, for balloon advection)" --> wind_backend
```

## Notes

- **sim-server is authoritative.** All balloon physics and radio-link detection
  (`link_detection.rs`, ported from the original `linkDetection.js`) run
  server-side in a single task that owns `World` exclusively — no locks.
  Mutations arrive as `Command`s over an `mpsc` channel; state goes out as
  JSON `Snapshot`s over a `broadcast` channel to every connected client.
- **The browser client is a thin renderer.** `src/main.js` holds no physics
  or link-detection logic; it reconciles Cesium entities against whatever
  `sim-server` broadcasts and forwards user actions as REST commands. It also
  supports click-to-inspect: clicking a balloon selects it and opens an
  inspector panel with its live lat/lon/altitude, refreshed each snapshot.
- **`sim-server` is the sole client of the weather backend.** It fetches the
  wind field once at startup (for advection) and falls back to zero wind if
  `wind_backend.py` isn't running. It holds that field as a shared `Arc` and
  re-serves it over its own `GET /api/wind-levels`, so the browser's
  (arrows-only) copy — slow to transfer, ~55s / 356MB, see
  `WIND_TRANSFER_PERF.md` — comes from `sim-server`, not from Python
  directly. One fetch to Python, one payload in memory.
- **Pause and slider values are broadcast, not local.** A `SetPaused`
  command and `paused` flag on `World` back a `POST /api/paused` endpoint;
  when paused, `World::tick` freezes physics/link recomputation. Both
  `paused` and `horizonRefractionCoeff` are included in every `Snapshot` so
  all connected tabs stay in sync — controls POST the desired value and wait
  for the server to echo it back rather than flipping optimistically.
- **`weather-data-server/get_wind_data.py`** is a separate, older prototype
  script (not part of the running app) — the live backend is `wind_backend.py`.

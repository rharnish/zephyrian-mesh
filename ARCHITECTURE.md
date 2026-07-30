# Architecture

Three processes, started together by `run-all.sh` (see [README.md](README.md#how-to-build-and-run)):

```mermaid
flowchart TB
    subgraph client["Browser — Vite + CesiumJS (src/)"]
        main["main.js<br/>orchestrator: init viewer, own<br/>the snapshot loop, wire panels"]
        simClient["simClient.js<br/>every sim-server conversation:<br/>ws stream in, REST commands out"]

        subgraph layers["Rendering layers"]
            balloonLayer["balloonLayer.js<br/>balloon entity reconciliation"]
            linkLayer["linkLayer.js<br/>radio-link polylines"]
            commsReplay["commsReplay.js<br/>bundle packet animation"]
            windVectors["windVectors.js<br/>wind arrow overlay"]
            towerRender["tower.js / towerModel.js"]
        end

        subgraph panels["UI panels (src/ui/)"]
            controlPanel["controlPanel.js<br/>controls + mesh health"]
            inspectorPanel["inspectorPanel.js<br/>per-balloon inspect,<br/>comms log"]
        end

        subgraph pure["Pure helpers (unit-tested)"]
            overlays["overlays.js<br/>overlay classification<br/>+ palette"]
            balloonIcon["balloonIcon.js<br/>glyph canvases"]
            cesiumColor["cesiumColor.js"]
            geo_js["geo.js"]
            config_js["config.js"]
        end

        main --> simClient
        main --> layers
        main --> panels
        layers --> pure
        panels --> pure
    end

    subgraph sim["sim-server — Rust / Axum (sim-server/src/)"]
        main_rs["main.rs<br/>owns the single World task,<br/>ws + REST routes"]
        sim_rs["sim.rs<br/>World: tick loop, Commands,<br/>Snapshot broadcast"]

        subgraph physics["Physics"]
            balloon_rs["balloon.rs<br/>advection + altitude"]
            atmosphere["atmosphere.rs<br/>ISA pressure/temp/density"]
            wind_field_rs["wind_field.rs"]
            tower_rs["tower.rs"]
        end

        subgraph comms["Comms protocol"]
            beacon["beacon.rs<br/>decentralized discovery:<br/>believed connectivity"]
            bundle["bundle.rs<br/>store-and-forward telemetry,<br/>relay queues, tower acks"]
            telemetry["telemetry.rs<br/>sensor block"]
            ablation["ablation.rs<br/>protocol variants"]
        end

        subgraph topo["True topology"]
            link_detection["link_detection.rs<br/>compute_grid_edges<br/>(+ brute-force oracle)"]
            spatial_grid["spatial_grid.rs"]
            union_find["union_find.rs"]
            geo_rs["geo.rs"]
        end

        config_rs["config.rs<br/>tunables"]

        main_rs --> sim_rs
        sim_rs --> physics
        sim_rs --> comms
        sim_rs --> topo
        sim_rs --> config_rs
        balloon_rs --> atmosphere
        link_detection --> spatial_grid
        link_detection --> geo_rs
    end

    subgraph wind["weather-data-server — Python / FastAPI"]
        wind_backend["wind_backend.py<br/>serves ERA5 pressure-level<br/>wind data via xarray<br/>(file + time step resolved<br/>from data/catalog.json)"]
    end

    simClient -- "WebSocket /ws<br/>(Snapshot: balloons, towers, edges, beliefs,<br/>mesh health, paused, serverTimeMs)" --> main_rs
    simClient -- "REST: /api/towers, /api/balloons/count,<br/>/api/horizon-coeff, /api/paused,<br/>/api/balloons/:id/comms" --> main_rs
    simClient -- "GET /api/wind-levels<br/>(background, for arrow overlay)" --> main_rs
    main_rs -- "GET /api/wind-levels<br/>(startup, for advection)" --> wind_backend
```

## Notes

- **sim-server is authoritative.** All balloon physics, radio-link detection, and
  the comms protocol run server-side in a single task that owns `World`
  exclusively — no locks. Mutations arrive as `Command`s over an `mpsc` channel;
  state goes out as JSON `Snapshot`s over a `broadcast` channel to every client.

- **The browser is a thin renderer.** It holds no simulation state — it reconciles
  Cesium entities against whatever `sim-server` broadcasts and forwards user
  actions as REST commands. Controls POST a desired value and wait for the server
  to echo it back rather than flipping optimistically, so every connected tab
  stays in sync.

- **Belief and truth are computed separately.** `link_detection.rs` computes the
  true connectivity graph; `beacon.rs` computes what each balloon *believes* it
  can reach, from beacons it actually received. Both ride on every `Snapshot`,
  which is what makes the belief-vs-truth overlay possible. See
  [`docs/design/MESH_COMMS_DESIGN.md`](docs/design/MESH_COMMS_DESIGN.md).

- **Snapshots carry `serverTimeMs`.** A client that falls behind is sent the
  newest snapshot rather than a backlog, and the frontend surfaces its own view
  lag. See [`docs/investigations/`](docs/investigations/) and the `diagnose-lag`
  skill.

- **`sim-server` is the sole client of the weather backend.** It fetches the wind
  field once at startup and falls back to zero wind if `wind_backend.py` isn't
  running. It holds that field as a shared `Arc` and re-serves it over its own
  `GET /api/wind-levels`, so the browser's arrows-only copy — slow to transfer,
  ~55s / 356MB, see
  [`docs/investigations/WIND_TRANSFER_PERF.md`](docs/investigations/WIND_TRANSFER_PERF.md)
  — comes from `sim-server`, not from Python directly.

- **One wind *time step* is served, out of however many the file holds.** The
  ERA5 downloads can cover a full day at hourly resolution, but a single
  stride-2 step is already ~350MB of JSON, so `wind_backend.py` pins one step at
  startup (`WIND_TIME_INDEX`) rather than shipping the span. Which file and step
  is resolved from `weather-data-server/data/catalog.json`, generated by
  `catalog_data.py` — nothing is hardcoded, and `GET /api/wind-levels` carries an
  additive `source` object saying what was resolved. If no data resolves at all,
  Python serves an analytic jet-stream field flagged `synthetic: true` instead of
  refusing to start, because the old failure mode — sim-server silently falling
  to `WindField::zero()` — looked like a physics bug rather than a missing file.

- **Offline experiment binaries** live in `sim-server/src/bin/` and share the
  simulation code through `lib.rs`: `protocol_sweep`, `bundle_delivery`,
  `beacon_convergence`, `mesh_depth`, `connectivity_sweep`, `telemetry_records`,
  and `timing` (which asserts the timing model can't rot). Results land in
  [`experiments/`](experiments/).

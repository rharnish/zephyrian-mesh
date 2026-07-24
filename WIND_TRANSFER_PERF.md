# Wind data transfer: backend → sim-server perf findings

Investigation into why fetching `/api/wind-levels` from `weather-data-server/wind_backend.py`
into `sim-server` is slow (~55s observed), and what could improve it.

## Current shape

- **Fetched once at process startup**, not per-tick or per-request during a run:
  - `sim-server/src/main.rs:76-96` — `fetch_wind_field()`, async via `reqwest::get`, no explicit timeout override.
  - `sim-server/src/bin/connectivity_sweep.rs:298-321` — `fetch_wind_field_blocking()`, `reqwest::blocking::Client` with an explicit 120s timeout (the plain default ~30s isn't enough — comment at `connectivity_sweep.rs:290-297`).
  - URL: `sim-server/src/config.rs:11` — `http://127.0.0.1:8000/api/wind-levels`.
  - No caching, retry, or local persistence on the Rust side. On any fetch/parse error, both call sites silently fall back to `WindField::zero()` (no wind) — no retry.
  - The JS frontend (`src/windField.js:8-19`) also fetches once via `fetch()`. The archived JS connectivity-sweep (`experiments/legacy-js/connectivity-sweep.mjs:33-35`) deliberately skips the real backend and uses zero wind — only the Rust sweep binary pays this cost.
  - Each independent sweep-shard process pays the full ~55s fetch separately (no shared cache across shards).

- **Deserialization target** (`sim-server/src/wind_field.rs:5-33`):
  ```
  WindField { header: WindHeader, levels: Vec<Level> }
  Level { pressure_hpa, altitude_m, u_data: Vec<Vec<f64>>, v_data: Vec<Vec<f64>> }
  ```
  Nested `Vec<Vec<f64>>` per level for both u and v — one heap allocation per row, per level, per component.

- **Raw JSON shape** (`weather-data-server/wind_backend.py:120-179`):
  ```json
  {
    "header": { "nx": int, "ny": int, "lo1": float, "la1": float, "lo2": float, "la2": float, "dx": float, "dy": float },
    "levels": [
      { "pressureHpa": float, "altitudeM": float, "u_data": [[float, ...] x ny], "v_data": [[float, ...] x ny] },
      ...
    ]
  }
  ```
  One entry per ERA5 pressure level, each a full `ny × nx` 2D grid for both u and v, spatially downsampled 2x (`SPATIAL_STRIDE = 2`, `wind_backend.py:28`) but otherwise the full grid, JSON-encoded as nested float arrays (no flattening, no binary/typed encoding).

- **Server-side caching is partial.** `_build_wind_levels_response()`'s output dict is cached in-process and built once at startup (`_wind_levels_response_cache`, `wind_backend.py:73,182-185`, preloaded via the `startup` event, `wind_backend.py:188-197` — see the `wind_backend_perf` fix from an earlier session). **But FastAPI still re-runs `json.dumps`/`jsonable_encoder` over the full cached nested-list structure on every request** — only the dict *construction* is cached, not the serialized bytes. This is likely the largest remaining chunk of the ~55s.

- **Post-fetch usage is not the bottleneck.** No conversion/indexing step happens after deserialization — `WindField::sample()` (`wind_field.rs:109-138`) reads the nested `Vec<Vec<f64>>` directly: a linear scan over `levels` (not binary search, despite being pre-sorted) plus bilinear interpolation via 8 nested-Vec lookups, called once per balloon per tick (400 balloons × 20 ticks/sec by default). Real but secondary compared to the one-time fetch/parse cost.

## Recommendations, by impact/effort

1. **Cache the serialized response bytes on the server, not just the dict.** Pre-encode the JSON once at startup (e.g. return a raw `Response(content=cached_bytes, media_type="application/json")`) instead of returning a dict FastAPI re-serializes every call. Likely the single biggest win since it eliminates repeated `json.dumps` over a huge nested float structure.
2. **Flatten and/or shrink the wire format.** Row-major flat `Vec<f64>` (or `f32`) per level instead of `Vec<Vec<f64>>` removes the nested-array JSON overhead and the per-row Rust allocations on parse; switching to `f32` halves payload size outright (wind data doesn't need float64 precision). A binary format like msgpack would go further, cutting both size and parse cost vs. JSON text.
3. **Enable response compression** (e.g. `GZipMiddleware`), ideally over the pre-cached bytes from #1 so compression itself is also paid once, not per request.
4. **Local fetch cache for repeated sweep runs.** Persist the fetched wind field to a local file after first fetch so repeated sim-server/sweep-shard startups can skip the network round-trip entirely — matters most for sweeps that spawn many shards, each currently paying the full ~55s independently.
5. **Downsample further for sim-server's use case**, e.g. a query param for coarser stride than the frontend needs, if full frontend resolution isn't required for simulation accuracy.
6. **On the Rust side**, mirror the flattened format in `wind_field.rs` (flat `Vec<f32>` + `nx`/`ny` index math) — smaller allocation count and better cache locality for `sample()`, though this is secondary to the transfer cost itself.

## Suggested next step

Implement #1 first — highest impact, lowest risk, isolated to `wind_backend.py`.

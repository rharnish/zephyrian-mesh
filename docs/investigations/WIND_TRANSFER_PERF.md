# Wind data transfer: backend → sim-server perf findings

Investigation into why fetching `/api/wind-levels` from `weather-data-server/wind_backend.py`
into `sim-server` was slow (~55s / ~356MB originally observed), and what could improve it.

**Update:** recs #1, #2 (partial), #3, and #4 are done. Measured end-to-end over real
HTTP (`curl`, not just in-process): ~356MB/~55s baseline → 146.2MB/17.7s after
the float32-rounding fix (#2) → **146.2MB/~0.04s** after also caching the
serialized bytes (#1), or **50.6MB/~0.02s** when the client sends
`Accept-Encoding: gzip` (#3). sim-server's `reqwest` client doesn't currently
enable the `gzip` Cargo feature, so it gets the uncompressed 146MB response —
still a ~99.7% time cut from the original, no Rust changes needed. Confirmed
working end-to-end via `run-all.sh` (`sim_server: loaded wind field from
http://127.0.0.1:8000/api/wind-levels`). Enabling gzip for sim-server too, plus
#2's remaining flattening/dtype half and #4/#5/#6, are still open.

## Current shape

- **Fetched once at process startup**, not per-tick or per-request during a run:
  - `fetch_wind_field()` in `sim-server/src/main.rs`, async via `reqwest::get`, no explicit timeout override.
  - `fetch_wind_field_blocking()` in `sim-server/src/bin/connectivity_sweep.rs`, `reqwest::blocking::Client` with an explicit 120s timeout (the plain default ~30s isn't enough — see the comment above it).
  - URL: `WIND_API_URL` in `sim-server/src/config.rs` — `http://127.0.0.1:8000/api/wind-levels`.
  - No caching, retry, or local persistence on the Rust side. On any fetch/parse error, both call sites silently fall back to `WindField::zero()` (no wind) — no retry.
  - The JS frontend (`WindField.fetchFromBackend` in `src/windField.js`) also fetches once via `fetch()`. The archived JS connectivity-sweep (`experiments/legacy-js/connectivity-sweep.mjs`) deliberately skips the real backend and uses zero wind — only the Rust sweep binary pays this cost.
  - Each independent sweep-shard process pays the full ~55s fetch separately (no shared cache across shards).

- **Deserialization target** (the `WindField` struct in `sim-server/src/wind_field.rs`):
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
  One entry per ERA5 pressure level, each a full `ny × nx` 2D grid for both u and v, spatially downsampled 2x (`DEFAULT_SPATIAL_STRIDE = 2` in `wind_backend.py`, now overridable per file via `wind_source.json`'s `spatialStride`) but otherwise the full grid, JSON-encoded as nested float arrays (no flattening, no binary/typed encoding).

- **Server-side caching now covers both the dict and the serialized bytes.** `_build_wind_levels_response()`'s output dict is cached in-process and built once at startup (`_wind_levels_response_cache`), and `_wind_levels_json_bytes()` now also pre-encodes it to compact-JSON bytes *and* gzip once at startup (`_wind_levels_bytes_cache`, `wind_backend.py`), both built in `preload_dataset()`. The `/api/wind-levels` endpoint serves the pre-made bytes directly via `Response(...)`, choosing gzip only if the request's `Accept-Encoding` asks for it — this was the largest remaining chunk of the original ~55s (re-running `json.dumps`/`jsonable_encoder` over ~19M floats on every request), confirmed by two back-to-back `curl` timings before the fix both taking ~17.7s.

- **Post-fetch usage is not the bottleneck.** No conversion/indexing step happens after deserialization — `WindField::sample()` (`wind_field.rs`) reads the nested `Vec<Vec<f64>>` directly: a linear scan over `levels` (not binary search, despite being pre-sorted) plus bilinear interpolation via 8 nested-Vec lookups, called once per balloon per tick (400 balloons × 20 ticks/sec by default). Real but secondary compared to the one-time fetch/parse cost.

## Recommendations, by impact/effort

1. **Done.** Cache the serialized response bytes on the server, not just the dict. `_wind_levels_json_bytes()` (`wind_backend.py`) pre-encodes to compact JSON once at startup and the endpoint returns those bytes via `Response(...)` instead of a dict FastAPI re-serializes every call. This was the single biggest win: ~17.7s/request → ~0.04s/request, measured via back-to-back `curl` timings.
2. **Flatten and/or shrink the wire format.** Row-major flat `Vec<f64>` (or `f32`) per level instead of `Vec<Vec<f64>>` removes the nested-array JSON overhead and the per-row Rust allocations on parse. A binary format like msgpack would go further, cutting both size and parse cost vs. JSON text.

   **Partially done** (`wind_backend.py` — `WIND_VALUE_DECIMALS` / `_round_wind_values()`): the source netCDF is float32, but `.values.tolist()` on a float32 array converts each element to a float64 that reproduces the float32 bit pattern exactly (e.g. `12.4f32` → `12.399999618530273`), and `json.dumps` then prints that float64's full ~17-digit shortest round-trip repr — text precision the data never had. Rounding to 4 decimals after casting to float64 (rounding a float32 array in place doesn't work — it stays float32 and re-upscales on the next `.tolist()`) cuts the measured payload from ~356MB to ~146MB (~59% measured over real HTTP) with no loss of real precision, no wire-format change, and no Rust-side changes required. The `f32`-vs-`f64` wire type and flattening are still open.
3. **Done.** Enable response compression. `_wind_levels_json_bytes()` also pre-gzips the same bytes once at startup (`compresslevel=1` — level 6 profiled slower for no useful size win); the endpoint serves the gzip bytes with `Content-Encoding: gzip` when the request's `Accept-Encoding` includes `gzip`, else the plain bytes — so compression is paid once at startup, not per request, either way. Cuts the wire size further to ~50.6MB when a client asks for it. **sim-server doesn't ask for it yet** — its `reqwest` client is built with `default-features = false` and only `["json", "rustls-tls", "blocking"]` (`sim-server/Cargo.toml`), so it never sends `Accept-Encoding: gzip` and gets the plain ~146MB response. Adding the `gzip` feature there would get sim-server the smaller payload too, at the cost of a Rust-side dependency change.
4. **Done.** Local fetch cache for repeated sweep runs. Shipped as `sim-server/src/wind_cache.rs` (plus the `wind_cache` management binary, the `--wind` flag, and `ZM_WIND_CACHE`): the fetched field is persisted to disk and reloaded on later startups, so sim-server and every sweep shard skip the network round-trip entirely. With a warm cache the Python backend need not run at all — `run-all.sh` skips starting it.
5. **Downsample further for sim-server's use case**, e.g. a query param for coarser stride than the frontend needs, if full frontend resolution isn't required for simulation accuracy.
6. **On the Rust side**, mirror the flattened format in `wind_field.rs` (flat `Vec<f32>` + `nx`/`ny` index math) — smaller allocation count and better cache locality for `sample()`, though this is secondary to the transfer cost itself.

## Suggested next step

#1, #3 and #4 are done and confirmed end-to-end via `run-all.sh`. #4 in particular changed the shape of the problem: with a warm wind cache the transfer does not happen at all on a normal run, so the remaining items only matter for a cold cache or a deliberate re-fetch. Of what's left, enabling the `gzip` Cargo feature on sim-server's `reqwest` client (small, isolated change, lets it pick up the already-built #3 gzip path for free) is the next lowest-effort win; #2's remaining flatten/dtype half and #6 are the larger, higher-effort items after that.

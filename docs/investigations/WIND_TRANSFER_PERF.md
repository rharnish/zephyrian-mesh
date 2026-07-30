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

---

## Addendum (2026-07-29): reading *one hour* out of a multi-hour file

Context: the data on disk is now a 24-hour ERA5 pressure-level download
(`valid_time=24`, 37 levels, 721x1440 — 8.0GB), and the plan is to advect
balloons through a *changing* wind field, interpolating between consecutive
hours at 1 simulated hour = 1 wind hour. See the "time-varying wind" stage in
the design notes.

**The measurement.** `/api/wind-levels/stats` at `WIND_SPATIAL_STRIDE=8`, which
reads u and v for all 37 levels of a single time step and computes speed stats,
touching no JSON serialization at all:

| time index | wall clock |
|---|---|
| 0 (1978-06-09T00:00) | 48.3 s |
| 12 (1978-06-09T12:00) | 48.3 s |

**This is disk/decode cost, not serialization cost, and the stride barely helps.**
Stride 8 versus stride 2 changes the *output* size 16x but not the ~334MB of
compressed netCDF that has to be read and decoded to produce one hour. That
makes it a different bottleneck from the ~55s JSON figure above, and it is
unaffected by every recommendation in the previous section.

**Why it matters.** `sim-server` ticks at 20Hz advancing `TICK_DT_SECONDS *
TIME_SCALE` = 15 simulated seconds per tick (`config.rs:6,7,244`), i.e. 300
simulated seconds per real second. At 1 sim hour = 1 wind hour, **a wind hour
elapses every 12 real seconds** and the whole 24-hour span is consumed in ~4.8
real minutes. A prefetch-the-next-hour design therefore has a ~12 s budget, and
the measured cost is ~48 s — **4x over budget**. Prefetching one hour ahead is
not enough on its own.

**Options, roughly in order of expected payoff:**

1. **Repack the span once into a contiguous per-hour layout** (flat `f32`,
   levels x lat x lon, one contiguous block per hour). Reading hour N becomes a
   seek plus a ~73MB sequential read — or an `mmap` — instead of decoding
   scattered netCDF chunks. Turns a 48 s cost into something well under the
   budget and is a natural extension of `catalog_data.py`. Costs ~1.8GB of disk
   for the 24-hour span and one preprocessing pass.
2. **Read whole chunks, not single hours.** *Measured, and this is the big one —
   see the chunk-layout finding below.*
3. **Prefetch deeper than one hour** — buys linear headroom (2 hours ahead = 24 s
   budget) but doesn't close a 4x gap alone, and multiplies resident memory.
4. **Lower `TIME_SCALE`, or set `WIND_SECONDS_PER_SIM_SECOND < 1.0`.** Cheapest
   possible fix, but it changes what the simulation *means* rather than making
   it faster, so it's a fallback, not a solution.
5. **Fewer pressure levels.** Balloons fly in roughly the 50-200hPa band; 8
   levels instead of 37 is a ~4.6x read reduction and would land inside the
   budget on its own. Requires deciding that the full profile isn't needed.

### The chunk layout explains most of the 48 s

`ds["u"].encoding` on the 8GB file:

```
dims             : ('valid_time', 'pressure_level', 'latitude', 'longitude')
shape            : (24, 37, 721, 1440)
chunksizes       : (5, 8, 181, 360)
zlib             : True,  complevel: 1,  dtype: float32
```

**Chunks span 5 time steps.** netCDF chunks are the unit of decompression, so
asking for one hour forces zlib decompression of a block covering *five* hours
and throws 80% of it away. Every single-hour read pays ~5x its own cost. That
accounts for nearly the whole gap: ~48 s / 5 ≈ 10 s of genuinely needed work,
comfortably inside the 12 s budget.

Two consequences that change the design:

- **Read chunk-aligned 5-hour blocks and keep all five.** One ~48 s read buys 5
  wind hours = 60 s of sim time at 1 sim hour = 1 wind hour. That is a positive
  margin (~1.25x) with no new file format at all — just aligning the prefetch to
  hour indices 0-4, 5-9, 10-14, ... instead of fetching hour-by-hour. Resident
  cost is 5 x ~73MB ≈ 365MB at stride 2, less at coarser strides.
- **The margin is thin, so combine it with a level subset.** Chunks are 8 levels
  deep; restricting to the ~8 levels covering the 50-200hPa flight band aligns
  with that too and is roughly another 4.6x. Together they turn this from
  "marginal" into "not a concern".

Note the data is already `float32` on disk, so recommendation #2 in the previous
section (f32 over the wire) is a pure win with no precision loss — the current
JSON path is *upcasting* to f64 and then printing it as text.

**Revised recommendation:** align prefetch to the 5-hour chunk boundary first —
it needs no new format and is a ~5x improvement. Add a pressure-level subset if
more headroom is wanted. Only reach for a repacked binary cube (option 1) if
those prove insufficient, since it adds a derived 1.8GB artifact and a
preprocessing step. **Do not build a naive hour-by-hour prefetch — it reads each
chunk five times over.**

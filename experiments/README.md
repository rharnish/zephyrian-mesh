# Connectivity sweep — how to run it, and how to run it again with different parameters

> This file is about the **connectivity sweep** specifically. Other
> experiments living in this directory:
>
> | Experiment | Write-up | Generator |
> |---|---|---|
> | Real dv-dtn protocol vs. omniscient connectivity (10 seeds per cell) | [`protocol-results/`](protocol-results/) (which also holds the density-sweep chart) | [`protocol_sweep.rs`](../sim-server/src/bin/protocol_sweep.rs) |
> | Batching and ack-digest aggregation | [`aggregation-summary.md`](aggregation-summary.md) | [`aggregation_sweep.rs`](../sim-server/src/bin/aggregation_sweep.rs) |
> | Every protocol over identical fields (incl. the wind coda) | [`aggregation-summary.md`](aggregation-summary.md) | [`protocol_compare.rs`](../sim-server/src/bin/protocol_compare.rs) |
> | Protocols crossed with real weather fields | [`aggregation-summary.md`](aggregation-summary.md) | [`wind_sweep.rs`](../sim-server/src/bin/wind_sweep.rs) |
> | Does wind actually churn the topology? | (in the above) | [`link_churn.rs`](../sim-server/src/bin/link_churn.rs) |
> | Tuning reactive + link-state (MPR, expanding ring, overhearing) | [`aggregation-summary.md`](aggregation-summary.md) Coda 2 | [`discovery_sweep.rs`](../sim-server/src/bin/discovery_sweep.rs) |
> | Delivery latency, not just delivery rate | [`aggregation-summary.md`](aggregation-summary.md) Coda 3 | [`discovery_sweep.rs`](../sim-server/src/bin/discovery_sweep.rs) |
> | Every protocol across balloon density, through percolation | [`aggregation-summary.md`](aggregation-summary.md) + [`protocol-results/`](protocol-results/) | [`density_sweep.rs`](../sim-server/src/bin/density_sweep.rs) |
> | Grounded-% ground truth over the same density grid | (the `--truth` overlay on the above) | [`ground_truth_sweep.rs`](../sim-server/src/bin/ground_truth_sweep.rs) |
> | This sweep (connectivity vs. balloon count and horizon coeff) | [`sweep-summary-rust.md`](sweep-summary-rust.md) | [`connectivity_sweep.rs`](../sim-server/src/bin/connectivity_sweep.rs) |
>
> **Results CSVs** live beside this file, one per experiment
> (`aggregation-sweep-results.csv`, `density-sweep-results.csv`,
> `discovery-sweep-results.csv`, `ground-truth-sweep-results.csv`,
> `protocol-sweep-results.csv`, `wind-sweep-results.csv`,
> `connectivity-sweep-results-rust.csv`). The tables and charts in the
> write-ups are regenerated from them by the `summarize_*.py` and `plot_*.py`
> scripts here — `summarize_aggregation.py`, `summarize_discovery.py`,
> `summarize_wind.py`, `summarize_sweep.py`, and `plot_sweep.py` /
> `plot_protocol_sweep.py` / `plot_density_sweep.py` (each with an
> `*_html.py` twin that emits an interactive chart instead of a PNG).
>
> Anything taking a `--wind` flag reads a **cached** field rather than
> fetching one, so runs are reproducible and need no Python backend. See
> [`wind_cache.rs`](../sim-server/src/bin/wind_cache.rs) and the "Wind" note
> in the root [README](../README.md).

**The Rust version (`connectivity_sweep.rs`) is the live implementation —
use it for new sweeps.** The original JS implementation has been archived to
[`legacy-js/`](legacy-js/) (kept runnable for reference/comparison, not
under active development) now that the app's simulation architecture has
fully moved to `sim-server`. See [`legacy-js/README.md`](legacy-js/README.md)
if you need to run it.

| | JS (archived) | Rust |
|---|---|---|
| File | `legacy-js/connectivity-sweep.mjs` | `../sim-server/src/bin/connectivity_sweep.rs` |
| Run via | `node experiments/legacy-js/connectivity-sweep.mjs <mode>` | `cargo run --release --bin connectivity_sweep -- <mode>` (from `sim-server/`) |
| Per-combo results | `experiments/legacy-js/results/` | `experiments/results-rust/` |
| Combined CSV (default) | `experiments/legacy-js/connectivity-sweep-results.csv` | `experiments/connectivity-sweep-results-rust.csv` |
| Parallelism | `run-shards.sh` (N processes, each re-fetching wind) | `full` mode itself (rayon, one process, one wind fetch) |
| Sweep grid | constants in `connectivity-sweep.mjs` | JSON config file (default `sweep-config.json`) |
| Speed (worst-case combo, 1 sim hour) | ~119s | **~28s (~4.25x faster)** |
| Balloon layout per combo | fresh `Math.random()` draw each run (not reproducible) | deterministic per-combo seed via `seed_for_combo()` (reproducible) |
| Wind | zero (static, no lateral drift) | **real**, fetched once per run from `wind_backend.py`, falls back to zero if unreachable |

Both implement the exact same model (see `legacy-js/connectivity-sweep.mjs`'s
header comment for the full description: payload generation every 5
sim-minutes, radio delivery after a continuous 30s grounded streak,
satellite fallback on timeout) and produce the same JSON/CSV shape, so
results from either are directly comparable. The JS version stays around as
the original/reference implementation and because it's the one that
produced `legacy-js/connectivity-sweep-results.csv` + `legacy-js/sweep-summary-js.md` — same
correctness as Rust (cross-checked via its `quick` mode output, same
trends/order of magnitude), just slower and no longer developed.

Both are self-contained and headless — no Cesium viewer, no `sim-server`
web process needed. JS's simulation building blocks
(`legacy-js/src/balloon.js`, `spatialGrid.js`, `unionFind.js`, `linkDetection.js`) now live
alongside it in the archive rather than in the live app's `src/` — they were
already dead code there once `sim-server` took over, kept only for this
sweep. Rust depends on `sim-server`'s lib target. The Rust version does need
`wind_backend.py` running if you want real wind (see below) — it works fine
without it too, just falls back to zero wind with a printed warning.

## Quick start

```bash
# Rust (recommended)
cd sim-server
cargo build --release --bin connectivity_sweep
./target/release/connectivity_sweep bench   # perf check, ~30s, no sweep
./target/release/connectivity_sweep quick    # tiny sweep, sanity check output shape, ~10s
cd ..
./sim-server/target/release/connectivity_sweep full   # the real sweep, reads experiments/sweep-config.json

# JS (archived, see legacy-js/README.md)
node experiments/legacy-js/connectivity-sweep.mjs bench
node experiments/legacy-js/connectivity-sweep.mjs quick
./experiments/legacy-js/run-shards.sh
```

`bench` estimates full-sweep cost before you commit to it. `quick` runs a
tiny 2x2 grid over 2 sim hours so you can sanity-check the output shape.
Neither writes to the results directories — only `full` does.

## Repeating the experiment with different parameters

**Rust (`connectivity_sweep.rs`):** the sweep grid lives in a JSON config
file, not in code — default path `experiments/sweep-config.json` (see
[`sweep-config.json`](sweep-config.json) and
[`sweep-config-full-grid.json`](sweep-config-full-grid.json) for the two
grids behind `sweep-summary-rust.md`):

```json
{
  "horizonCoeffs": [3.4, 3.6, 3.8, 4.0],
  "nBalloons": [50, 100, 200, 400, 800, 1600],
  "fallbackTimeoutMin": [10, 20, 30, 60],
  "ackDurationSec": 30,
  "durationHours": 24,
  "resultsDir": "experiments/results-rust",
  "outCsv": "experiments/connectivity-sweep-results-rust.csv"
}
```

Edit this (or pass a different file as the first argument to `full`), then
re-run — no rebuild needed:

```bash
./sim-server/target/release/connectivity_sweep full                                    # default config path
./sim-server/target/release/connectivity_sweep full experiments/my-sweep-config.json    # custom grid
```

`ackDurationSec`, `durationHours`, `resultsDir`, and `outCsv` are all
optional and default to the values shown above. To sweep a **new
parameter** (e.g. ack duration itself, currently a single fixed value): add
it as an array to `SweepConfig`, add another nested loop in `combos()`, add
the field to `Combo` and `ResultRow`, and thread it through
`run_combo`/`run_one` the same way `fallback_timeout_min` already is.
`combo_file_name()` already encodes `ack_duration_sec` in the filename (the
`_a{}` suffix), so per-combo files won't collide once it actually varies.

**JS (`legacy-js/connectivity-sweep.mjs`)** still takes its grid from
constants near the top of the file — edit those, then re-run.

**Per-combo JSON files get archived into `resultsDir/json/` once combined.**
Both `full` mode's auto-combine step at the end of a run and standalone
`combine` move every per-combo JSON file into a `json/` subfolder after
folding it into the CSV (see `archive_combo_jsons`), so `resultsDir`'s top
level doesn't fill up with hundreds of tiny files — only the CSV-adjacent
outputs (e.g. `chart-data.json`, chart HTML/PNG) stay there. The
resume-after-kill skip check (see the comment above `write_combo_json`)
looks in both the flat directory and `json/`, so archived combos are still
recognized as done if you point a later run at the same `resultsDir`.

**Important — use a fresh results directory per experiment.** If you change
the parameter grid but leave old JSON files (flat or archived) sitting in
`experiments/results-rust/`, stale combos from a previous parameter set
won't get overwritten or flagged as wrong — they'll just silently persist
and get folded into the combined CSV. Before a new experiment with
different parameters, either clear the directory:

```bash
rm -rf experiments/results-rust    # or legacy-js/results/ for the JS version
```

or point `resultsDir`/`outCsv` in the config at a new location for that
experiment, e.g. `experiments/results-rust-v2/` with a matching CSV name,
so old and new experiments don't mix.

## Parallelism

**Rust:** `full` mode parallelizes across combos in-process with
[rayon](https://docs.rs/rayon), using all available cores by default
(override with the `RAYON_NUM_THREADS` env var). Wind is fetched once for
the whole run, not once per thread. Kill and re-run at any time —
already-done combos (existing JSON files) are skipped on restart, so
nothing already computed is lost. Being one process makes it
straightforward to profile — e.g. `perf record -g -- ./target/release/connectivity_sweep full`
or `cargo flamegraph --bin connectivity_sweep -- full`.

**JS:** `run-shards.sh` still splits the combo grid across `nproc` parallel
*processes* by default (override with the first argument), each handling
every Nth combo and each fetching wind independently.

```bash
./experiments/legacy-js/run-shards.sh 4                          # 4 shards, default output path
./experiments/legacy-js/run-shards.sh 8 experiments/my-run.csv    # 8 shards, custom CSV name
```

## Wind: real in Rust, zero in JS

**JS (`legacy-js/connectivity-sweep.mjs`) still uses a static zero-wind field** —
balloons hold their spawn (lon, lat) for the entire run; only altitude
drifts (via each balloon's own target-altitude controller). This was a
deliberate simplification when `wind_backend.py` wasn't reliably available
in the sweep's original environment (see the `wind_backend_perf` memory)
and the JS version was never updated. If you need to compare directly
against the old `legacy-js/connectivity-sweep-results.csv` / `legacy-js/sweep-summary-js.md`,
keep using the JS version so the comparison is apples-to-apples.

**The Rust version fetches real wind data** from `wind_backend.py` once at
the start of each run (`fetch_wind_field_blocking()` in
`connectivity_sweep.rs`), the same endpoint and same
fall-back-to-zero-wind-on-failure behavior as `sim-server`'s main binary.
With real wind, balloons drift laterally the same way they do in the live
app — a real behavior difference from the JS version and from Rust's own
earlier zero-wind runs, not just a perf change. If `wind_backend.py` isn't
running, you'll see:

```
Failed to fetch wind field from http://127.0.0.1:8000/api/wind-levels (is wind_backend.py running?), using zero wind: ...
```

printed to stderr, and the run proceeds with zero wind anyway (same
behavior as before this was wired up). Note the fetch uses an explicit
120s timeout — the plain `reqwest::blocking::get()` shorthand's ~30s
default timeout is shorter than this endpoint's measured ~55s response
time (356MB payload — see `wind_backend_perf` memory), and would otherwise
silently and confusingly fall back to zero wind on every run even with
`wind_backend.py` healthy and running.

For Rust's `full` mode: wind is fetched **once per run**, not once per
combo or once per thread — the fetched field is shared read-only across
all of rayon's worker threads. For JS's `run-shards.sh`: wind is fetched
once per shard *process*, so an 8-shard run costs 8 fetches total (~55s ×
8, potentially all in parallel if `wind_backend.py` can serve concurrent
requests).

Either way, real wind-driven lateral drift lets balloons move into and out
of range of each other and of towers, which could change the connectivity
threshold compared to the zero-wind numbers in `sweep-summary-js.md` — flag
this before comparing across the two directly; relative comparisons across
swept parameters *within* one version's results should still be
directionally meaningful.

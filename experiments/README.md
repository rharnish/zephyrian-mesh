# Connectivity sweep — how to run it, and how to run it again with different parameters

Two implementations of the same experiment, kept side by side:

| | JS | Rust |
|---|---|---|
| File | `connectivity-sweep.mjs` | `../sim-server/src/bin/connectivity_sweep.rs` |
| Run via | `node experiments/connectivity-sweep.mjs <mode>` | `cargo run --release --bin connectivity_sweep -- <mode>` (from `sim-server/`) |
| Per-combo results | `experiments/results/` | `experiments/results-rust/` |
| Combined CSV (default) | `experiments/connectivity-sweep-results.csv` | `experiments/connectivity-sweep-results-rust.csv` |
| Shard script | `run-shards.sh` | `run-shards-rust.sh` |
| Speed (worst-case combo, 1 sim hour) | ~119s | **~28s (~4.25x faster)** |
| Balloon layout per combo | fresh `Math.random()` draw each run (not reproducible) | deterministic per-combo seed via `seed_for_combo()` (reproducible) |
| Wind | zero (static, no lateral drift) | **real**, fetched once per run from `wind_backend.py`, falls back to zero if unreachable |

Both implement the exact same model (see `connectivity-sweep.mjs`'s header
comment for the full description: payload generation every 5 sim-minutes,
radio delivery after a continuous 30s grounded streak, satellite fallback
on timeout) and produce the same JSON/CSV shape, so results from either are
directly comparable. **Use the Rust version for new sweeps** — same
correctness (cross-checked against the JS version's `quick` mode output,
same trends/order of magnitude), meaningfully faster. The JS version stays
as the original/reference implementation and because it's the one that
produced `connectivity-sweep-results.csv` + `sweep-summary.md`.

Both are self-contained and headless — no Cesium viewer, no `sim-server`
web process needed. They reuse the app's pure simulation logic directly
(JS imports `src/*.js`; Rust depends on `sim-server`'s lib target) but run
entirely standalone. The Rust version does need `wind_backend.py` running
if you want real wind (see below) — it works fine without it too, just
falls back to zero wind with a printed warning.

## Quick start

```bash
# Rust (recommended)
cd sim-server
cargo build --release --bin connectivity_sweep
./target/release/connectivity_sweep bench   # perf check, ~30s, no sweep
./target/release/connectivity_sweep quick    # tiny sweep, sanity check output shape, ~10s
cd ..
./experiments/run-shards-rust.sh             # the real sweep, sharded across all cores

# JS (original)
node experiments/connectivity-sweep.mjs bench
node experiments/connectivity-sweep.mjs quick
./experiments/run-shards.sh
```

`bench` estimates full-sweep cost before you commit to it. `quick` runs a
tiny 2x2 grid over 2 sim hours so you can sanity-check the output shape.
Neither writes to the results directories — only `full` (and thus the
shard scripts) does.

## Repeating the experiment with different parameters

Everything about the sweep is defined by a handful of constants near the
top of each file. To run a new experiment, edit these, then re-run — no
other code changes needed.

**In `connectivity_sweep.rs`** (and the equivalent names in
`connectivity-sweep.mjs`):

```rust
const PAYLOAD_INTERVAL_SEC: i64 = 5 * 60;      // how often each balloon generates a payload
const FIXED_ACK_DURATION_SEC: i64 = 30;         // continuous grounded streak needed for radio delivery

const HORIZON_COEFFS: [f64; 4] = [3.4, 3.6, 3.8, 4.0];   // km per sqrt(m) — radio horizon steepness
const N_BALLOONS: [u32; 6] = [50, 100, 200, 400, 800, 1600];
const FALLBACK_TIMEOUT_MIN: [i64; 4] = [10, 20, 30, 60]; // minutes before a pending payload gives up on radio
```

And the sim-hours-per-combo, set per mode in `main()`'s `match`:

```rust
"full" => {
    let duration_sec = 24 * 60 * 60; // 24 sim hours — change this to run shorter/longer combos
    ...
```

To sweep a **new parameter** (e.g. ack duration, currently fixed at 30s):
add it as another array (`ACK_DURATIONS_SEC: [i64; N]`) and another nested
loop in `combos()`, add the field to `Combo` and `ResultRow`, and thread it
through `run_combo`/`run_one` the same way `fallback_timeout_min` already
is. `combo_file_name()` already encodes `ack_duration_sec` in the filename
(the `_a{}` suffix), so per-combo files won't collide once it actually
varies.

**Important — use a fresh results directory per experiment.** `full` mode
skips any combo whose JSON file already exists in `RESULTS_DIR` (that's the
resume-after-kill feature — see the comment above `write_combo_json`), so
if you change the parameter grid but leave old JSON files sitting in
`experiments/results-rust/`, stale combos from a previous parameter set
won't get overwritten or flagged as wrong — they'll just silently persist
and get folded into `combine`'s CSV. Before a new experiment with different
parameters:

```bash
rm -rf experiments/results-rust    # or results/ for the JS version
```

or point `RESULTS_DIR` (Rust) / `RESULTS_DIR` (JS, same constant name) at a
new directory for that experiment, e.g. `experiments/results-rust-v2/`, and
give the combined CSV a matching name so old and new experiments don't mix:

```bash
./experiments/run-shards-rust.sh $(nproc) experiments/my-new-experiment.csv
```

(the shard script's second argument is the output CSV path — it doesn't
change `RESULTS_DIR` itself, so if you want fully separate per-combo JSON
too, edit `RESULTS_DIR` in `connectivity_sweep.rs` and rebuild.)

## Sharding

Both `run-shards.sh` and `run-shards-rust.sh` split the combo grid across
`nproc` parallel processes by default (override with the first argument),
each handling every Nth combo. Kill and re-run at any time — already-done
combos (existing JSON files) are skipped, so nothing already computed is
lost.

```bash
./experiments/run-shards-rust.sh 4                          # 4 shards, default output path
./experiments/run-shards-rust.sh 8 experiments/my-run.csv    # 8 shards, custom CSV name
```

## Wind: real in Rust, zero in JS

**JS (`connectivity-sweep.mjs`) still uses a static zero-wind field** —
balloons hold their spawn (lon, lat) for the entire run; only altitude
drifts (via each balloon's own target-altitude controller). This was a
deliberate simplification when `wind_backend.py` wasn't reliably available
in the sweep's original environment (see the `wind_backend_perf` memory)
and the JS version was never updated. If you need to compare directly
against the old `connectivity-sweep-results.csv` / `sweep-summary.md`,
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

For `full` mode / the shard scripts: wind is fetched **once per shard
process**, not once per combo — an 8-shard run costs 8 fetches total
(~55s × 8, potentially all in parallel if `wind_backend.py` can serve
concurrent requests), not 96.

Either way, real wind-driven lateral drift lets balloons move into and out
of range of each other and of towers, which could change the connectivity
threshold compared to the zero-wind numbers in `sweep-summary.md` — flag
this before comparing across the two directly; relative comparisons across
swept parameters *within* one version's results should still be
directionally meaningful.

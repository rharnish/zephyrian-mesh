#!/usr/bin/env bash
# Rust equivalent of run-shards.sh — launches the connectivity_sweep binary
# across N parallel shards (default: all available cores). Each combo
# writes its own JSON file to experiments/results-rust/ as it finishes;
# once all shards are done, this combines every JSON file into one CSV.
#
# Usage: experiments/run-shards-rust.sh [shardCount] [outCsvPath]
set -euo pipefail
cd "$(dirname "$0")/.."

SHARD_COUNT="${1:-$(nproc)}"
OUT_CSV="${2:-experiments/connectivity-sweep-results-rust.csv}"

echo "Building connectivity_sweep (release)..."
(cd sim-server && cargo build --release --bin connectivity_sweep)
BIN=sim-server/target/release/connectivity_sweep

echo "Per-combo JSON results will land in experiments/results-rust/ as each combo finishes."
echo "Launching $SHARD_COUNT shard(s)..."
mkdir -p experiments/results-rust
pids=()
for ((i = 0; i < SHARD_COUNT; i++)); do
  log="experiments/results-rust/shard${i}.log"
  "$BIN" full "$i" "$SHARD_COUNT" > "$log" 2>&1 &
  pids+=($!)
  echo "  shard $i -> pid ${pids[$i]}, log: $log"
done

echo "Waiting for all shards to finish (tail -f experiments/results-rust/shard0.log to watch progress)..."
for pid in "${pids[@]}"; do
  wait "$pid"
done

echo "All shards done. Combining JSON results into $OUT_CSV"
"$BIN" combine "$OUT_CSV"

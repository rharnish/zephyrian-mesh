#!/usr/bin/env bash
# Launches the connectivity sweep across N parallel shards (default: all
# available cores). Each combo writes its own JSON file to
# experiments/legacy-js/results/ as it finishes; once all shards are done, this
# combines every JSON file into one CSV.
#
# Usage: experiments/legacy-js/run-shards.sh [shardCount] [outCsvPath]
set -euo pipefail
cd "$(dirname "$0")/../.."

SHARD_COUNT="${1:-$(nproc)}"
OUT_CSV="${2:-experiments/legacy-js/connectivity-sweep-results.csv}"

echo "Per-combo JSON results will land in experiments/legacy-js/results/ as each combo finishes."
echo "Launching $SHARD_COUNT shard(s)..."
pids=()
for ((i = 0; i < SHARD_COUNT; i++)); do
  log="experiments/legacy-js/results/shard${i}.log"
  mkdir -p experiments/legacy-js/results
  node experiments/legacy-js/connectivity-sweep.mjs full "$i" "$SHARD_COUNT" > "$log" 2>&1 &
  pids+=($!)
  echo "  shard $i -> pid ${pids[$i]}, log: $log"
done

echo "Waiting for all shards to finish (tail -f experiments/legacy-js/results/shard0.log to watch progress)..."
for pid in "${pids[@]}"; do
  wait "$pid"
done

echo "All shards done. Combining JSON results into $OUT_CSV"
node experiments/legacy-js/connectivity-sweep.mjs combine "$OUT_CSV"

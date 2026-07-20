#!/usr/bin/env bash
# Launches wind_backend.py, sim-server, and the Vite frontend, in order,
# waiting for each to be ready before starting the next. Runs the frontend
# in the foreground so its output is visible here; Ctrl-C (or any exit)
# stops all three together. See RUNNING.md for the manual/three-terminal
# version of this same sequence.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

LOG_DIR="$(mktemp -d)"
echo "Logs: $LOG_DIR"

PIDS=()
cleanup() {
  echo
  echo "Stopping..."
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT INT TERM

port_open() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && exec 3<&- 3>&-
}

wait_for_log() {
  local logfile=$1 pattern=$2 name=$3 timeout=${4:-60}
  echo "Waiting for $name..."
  for ((i = 0; i < timeout; i++)); do
    grep -q "$pattern" "$logfile" 2>/dev/null && { echo "$name is up."; return 0; }
    sleep 1
  done
  echo "Timed out waiting for $name — check $logfile" >&2
  exit 1
}

# --- 1. wind_backend.py (port 8000) ---------------------------------------
if port_open 8000; then
  echo "Something is already listening on :8000 — assuming wind_backend.py is up, skipping."
else
  echo "Starting wind_backend.py..."
  (cd weather-data-server && exec ./run.sh) > "$LOG_DIR/wind_backend.log" 2>&1 &
  PIDS+=($!)
  wait_for_log "$LOG_DIR/wind_backend.log" "Application startup complete" "wind_backend.py" 60
fi

# --- 2. sim-server (port 8080) ---------------------------------------------
if port_open 8080; then
  echo "Something is already listening on :8080 — assuming sim-server is up, skipping."
else
  echo "Starting sim-server (building first if needed — can take a minute)..."
  (
    cd sim-server \
      && cargo build --release \
      && exec ./target/release/sim-server
  ) > "$LOG_DIR/sim-server.log" 2>&1 &
  PIDS+=($!)
  wait_for_log "$LOG_DIR/sim-server.log" "listening on" "sim-server" 180
fi

# --- 3. frontend (port 5173, foreground) -----------------------------------
echo "Starting frontend — press Ctrl-C to stop everything."
npm run dev

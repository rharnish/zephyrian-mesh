#!/usr/bin/env bash
# Launches wind_backend.py, sim-server, and the Vite frontend, in order,
# waiting for each to be ready before starting the next. Runs the frontend
# in the foreground so its output is visible here; Ctrl-C (or any exit)
# stops all three together. See README.md for the manual/three-terminal
# version of this same sequence.
#
# Usage: ./run-all.sh [--local] [-h|--help]
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
  cat <<'EOF'
Usage: ./run-all.sh [--local] [-h|--help]

  (default)  dev mode — everything bound to 127.0.0.1, only reachable from
             this computer.
  --local    wind_backend.py and the Vite dev server bind to 0.0.0.0, so
             another computer on the LAN can open
             http://<this-machine's-LAN-IP>:5173 and reach everything
             (sim-server already binds 0.0.0.0). The frontend reads back
             whatever hostname it was loaded from (see src/config.js), so no
             other change is needed. Also opens 5173/8000/8080 in ufw
             (asking for sudo) and closes them again on exit.
  -h, --help Show this help and exit.
EOF
}

MODE="dev"
for arg in "$@"; do
  case "$arg" in
    --local) MODE="local" ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "Unknown option: $arg" >&2
      usage >&2
      exit 1
      ;;
  esac
done
echo "Mode: $MODE"

LAN_PORTS=(5173 8000 8080)

open_lan_ports() {
  echo "Opening ${LAN_PORTS[*]} in ufw for LAN access (sudo)..."
  for port in "${LAN_PORTS[@]}"; do
    sudo ufw allow "$port/tcp" || echo "Warning: failed to open $port/tcp in ufw" >&2
  done
}

close_lan_ports() {
  echo "Closing ${LAN_PORTS[*]} in ufw (sudo)..."
  for port in "${LAN_PORTS[@]}"; do
    sudo ufw delete allow "$port/tcp" 2>/dev/null || true
  done
}

if [[ "$MODE" == "local" ]]; then
  open_lan_ports
fi

LOG_DIR="$(mktemp -d)"
echo "Logs: $LOG_DIR"

PIDS=()
cleanup() {
  echo
  echo "Stopping..."
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [[ "$MODE" == "local" ]]; then
    close_lan_ports
  fi
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
  (
    [[ "$MODE" == "local" ]] && export WIND_BACKEND_HOST=0.0.0.0
    cd weather-data-server && exec ./run.sh
  ) > "$LOG_DIR/wind_backend.log" 2>&1 &
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
if [[ "$MODE" == "local" ]]; then
  npm run dev -- --host
else
  npm run dev
fi

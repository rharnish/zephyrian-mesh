#!/usr/bin/env bash
# Launches wind_backend.py, sim-server, and the Vite frontend, in order,
# waiting for each to be ready before starting the next. Runs the frontend
# in the foreground so its output is visible here; Ctrl-C (or any exit)
# stops all three together. See README.md for the manual/three-terminal
# version of this same sequence.
#
# Usage: ./run-all.sh [--local] [--protocol SPEC] [-h|--help]
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
  cat <<'EOF'
Usage: ./run-all.sh [--local] [--protocol SPEC] [-h|--help]

  (default)  dev mode — everything bound to 127.0.0.1, only reachable from
             this computer.
  --local    wind_backend.py and the Vite dev server bind to 0.0.0.0, so
             another computer on the LAN can open
             http://<this-machine's-LAN-IP>:5173 and reach everything
             (sim-server already binds 0.0.0.0). The frontend reads back
             whatever hostname it was loaded from (see src/config.js), so no
             other change is needed. Also opens 5173/8000/8080 in ufw
             (asking for sudo) and closes them again on exit.
  --protocol SPEC
             Which comms protocol sim-server runs — see PROTOCOLS below.
             Defaults to "dv-dtn" (the shipped configuration). Ignored if
             sim-server is already running on :8080.
  -h, --help Show this help and exit.

PROTOCOLS

  A "protocol" here is the whole answer to two questions: how does a balloon
  find out it can reach the ground, and how does data actually get there. The
  simulator owns physics, radio range and ground truth; the protocol owns
  everything else, including its own per-balloon state.

  The frontend needs no flag of its own. Each snapshot carries what the
  running protocol can express, and the UI hides anything it has no concept of
  — so a protocol without route beliefs simply shows no belief overlay rather
  than showing one full of meaningless values.

  SPEC is a protocol name, optionally followed by ':' and comma-separated
  key=value overrides. An unknown name or key is an error, not a fallback.

  dv-dtn   (the default, and currently the only one)
           Distance-vector discovery feeding store-and-forward delivery.
           Towers periodically flood hop-counted beacons; a balloon that hears
           one records a route belief and passes it on one hop further on its
           own duty-cycle slot. Telemetry bundles are then carried hop by hop
           along whatever next hop the holder currently *believes* in — which
           may be stale, and may be wrong. Nothing consults the true topology
           on a balloon's behalf; that is the point of the exercise.

  Overrides, and what they are for:

    ack=source-routed|digest      (default source-routed)
        How a delivery is acknowledged. source-routed sends a receipt back
        along the bundle's recorded path, one hop per wake slot, competing
        with ordinary forwarding for those slots. digest instead has towers
        announce recent deliveries inside beacons they were already sending,
        so the receipt costs no extra transmissions at all.
        Measured (single seed, n=1200): completion 66% -> 80%, and ack loss
        to zero by construction.

    mesh=N                        (default 1)
        Bundles carried per balloon-to-balloon hop. 1 treats a wake as one
        transmission. Raising it asks whether the mesh is limited by airtime
        or by opportunity. Measured: with ack=digest, mesh=4 reaches ~96%
        completion, against 66% for the shipped defaults.

    tower=N                       (default 4)
        Bundles handed over per tower contact. The measured last-hop lever:
        only ~23 of 1200 balloons can hear a tower at once, so this sets the
        ceiling everything else runs into. Set to 1 to reproduce the older
        one-bundle-per-contact behaviour.

    metric=freshest|nearest       (default freshest)
        Which route wins when two compete. freshest takes the newer wave
        however long its path; nearest prefers fewer hops. nearest helps
        below the percolation threshold and hurts above it, which is where
        the mesh usually sits.

    queue=fifo|lifo               (default fifo)
        Which held bundle moves when a slot comes up. lifo serves the newest
        first, so what moves still has budget left — at the cost of starving
        the bottom of the queue.

    originate=N                   (default 200)
        Rounds between a balloon originating telemetry. The demand knob.

    digest_entries=N              (default 16)
        With ack=digest, how many deliveries a tower announces per beacon.

  Examples:

    ./run-all.sh --protocol dv-dtn:ack=digest
    ./run-all.sh --protocol dv-dtn:ack=digest,mesh=4
    ./run-all.sh --protocol dv-dtn:metric=nearest,queue=lifo
EOF
}

MODE="dev"
# Explicit rather than empty, so a bare ./run-all.sh still prints what it is
# running. This is the *shipped* configuration, deliberately: the digest and
# batching variants measure better (see --help) but only at a single seed so
# far, and the default should be the honest baseline until that is confirmed
# across seeds. It is also the more interesting thing to watch — belief drift
# and satellite fallback are both plainly visible at these settings.
PROTOCOL="dv-dtn"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) MODE="local"; shift ;;
    --protocol)
      [[ $# -ge 2 ]] || { echo "--protocol needs a value" >&2; usage >&2; exit 1; }
      PROTOCOL="$2"; shift 2
      ;;
    --protocol=*) PROTOCOL="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done
echo "Mode: $MODE"
echo "Protocol: $PROTOCOL"

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
  echo "  NOTE: --protocol has no effect on an already-running sim-server." >&2
else
  echo "Starting sim-server (building first if needed — can take a minute)..."
  (
    cd sim-server \
      && cargo build --release --bin sim-server \
      && exec ./target/release/sim-server ${PROTOCOL:+--protocol "$PROTOCOL"}
  ) > "$LOG_DIR/sim-server.log" 2>&1 &
  PIDS+=($!)
  wait_for_log "$LOG_DIR/sim-server.log" "listening on" "sim-server" 180
fi

# --- 3. frontend (port 5173, foreground) -----------------------------------
# Unlike the other two, a busy :5173 is not something to skip past: Vite would
# quietly bind 5174 instead, and you would end up looking at a second frontend
# while assuming it was this one.
if port_open 5173; then
  echo "Something is already listening on :5173 — stop it first, or open that one." >&2
  echo "  (it will serve this same source; only sim-server's protocol differs)" >&2
  exit 1
fi
echo "Starting frontend — press Ctrl-C to stop everything."
if [[ "$MODE" == "local" ]]; then
  npm run dev -- --host
else
  npm run dev
fi

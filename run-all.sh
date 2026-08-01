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

  dv-dtn   (the default)
           Distance-vector discovery feeding store-and-forward delivery.
           Towers periodically flood hop-counted beacons; a balloon that hears
           one records a route belief and passes it on one hop further on its
           own duty-cycle slot. Telemetry bundles are then carried hop by hop
           along whatever next hop the holder currently *believes* in — which
           may be stale, and may be wrong. Nothing consults the true topology
           on a balloon's behalf; that is the point of the exercise.

  epidemic Binary spray-and-wait: no routes at all. A bundle starts with a
           copy budget, and each handoff gives half of it away, so the number
           of copies is bounded network-wide. Delivery is whichever copy
           happens to drift within earshot of a tower.
           Measured at 11-23% against dv-dtn's 72% — but that is replication
           run in the regime it suits worst, since links here are quasi-static
           and a maintained route stays valid. Included as the contrast.
           The UI degrades accordingly: no belief overlay, no comms replay,
           because the protocol declares it has no concept of either.

  Overrides, and what they are for (dv-dtn unless noted):

    ack=source-routed|digest      (default source-routed)
        How a delivery is acknowledged. source-routed sends a receipt back
        along the bundle's recorded path, one hop per wake slot, competing
        with ordinary forwarding for those slots. digest instead has towers
        announce recent deliveries inside beacons they were already sending,
        so the receipt costs no extra transmissions at all.
        Measured over 24 seeds at n=1200: +11.1 +/- 1.8 points of completion,
        and ack loss to zero by construction. The second half matters more
        than the first: under source-routed acks at mesh=8, 55% of bundles
        that *did* arrive left their origin believing they had not.

    mesh=N                        (default 1)
        Bundles carried per balloon-to-balloon hop. 1 treats a wake as one
        transmission. Raising it asks whether the mesh is limited by airtime
        or by opportunity. The larger of the two aggregation levers: worth
        +24.8 +/- 3.8 points over 24 seeds. With ack=digest, mesh=4 reaches
        94% completion against 63% for the shipped defaults. The two levers
        are substitutes, not complements — each is worth less once the other
        is in place, because both buy back the same wake slots.

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

    discovery=proactive|reactive  (default proactive)
        When routes are found. proactive maintains them continuously via
        tower beacons, so a balloon usually has a route already and it may be
        stale. reactive (AODV-style) spends nothing until there is a bundle
        to send, then floods a request and waits for a reply — under a duty
        cycle that is hops x the wake interval in each direction.
        Measured at 57% against proactive's 72%: the cost is waiting, not
        route quality. Incompatible with ack=digest, which needs tower
        beacons to ride on.

    reply=intermediate|tower      (default intermediate)
        With discovery=reactive, who may answer a request. intermediate lets
        any node holding a live route answer, which is what AODV does.
        tower restricts it to nodes that can hear a tower directly, so every
        route is built from first-hand knowledge. Worth only ~3 points here,
        and it makes requests travel further.

    copies=N                      (epidemic only, default 4)
        Starting copy budget per bundle, halved at each handoff. Raising it
        buys reach at the cost of congesting the queues the copies need.

  Examples:

    ./run-all.sh --protocol dv-dtn:ack=digest
    ./run-all.sh --protocol dv-dtn:ack=digest,mesh=4
    ./run-all.sh --protocol dv-dtn:metric=nearest,queue=lifo
    ./run-all.sh --protocol dv-dtn:discovery=reactive
    ./run-all.sh --protocol epidemic:copies=16

  The measured figures above come from experiments/aggregation-summary.md.
EOF
}

MODE="dev"
# Explicit rather than empty, so a bare ./run-all.sh still prints what it is
# running. This is the *shipped* configuration, deliberately. The digest and
# batching variants now measure better across 24 seeds rather than one, so the
# earlier reason for holding back is gone — but the default stays here because
# it is the honest baseline every experiment is quoted against, and because it
# is the more interesting thing to watch: belief drift and satellite fallback
# are both plainly visible at these settings and largely vanish at 94%
# completion. Changing the default would quietly re-baseline every figure in
# experiments/.
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

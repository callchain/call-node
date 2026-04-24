#!/usr/bin/env bash
# Start a 6-node Callchain devnet using the locally compiled `./target/release/calld`
# binary directly (no Docker). Validators are nodes 1-4, full nodes are 5-6.
#
# All processes:
#   - bind to 127.0.0.1 (loopback only),
#   - write data under devnet/local/data/nodeN/,
#   - log to devnet/local/logs/nodeN.log,
#   - have their PIDs recorded in devnet/local/run/nodeN.pid.
#
# Usage:
#   ./devnet/local/scripts/start.sh
#
# Environment:
#   CALLD_BIN    path to the calld binary (default: ./target/release/calld)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LOCAL_DIR="$ROOT_DIR/devnet/local"

CALLD_BIN="${CALLD_BIN:-$ROOT_DIR/target/release/calld}"

if [[ ! -x "$CALLD_BIN" ]]; then
    echo "ERROR: calld binary not found or not executable: $CALLD_BIN" >&2
    echo "Build it first with:" >&2
    echo "    cargo build --release -p call-node" >&2
    exit 1
fi

mkdir -p "$LOCAL_DIR/logs" "$LOCAL_DIR/run"
for i in 1 2 3 4 5 6; do
    mkdir -p "$LOCAL_DIR/data/node$i"
done

# Refuse to start if any node is already running.
for i in 1 2 3 4 5 6; do
    pid_file="$LOCAL_DIR/run/node$i.pid"
    if [[ -f "$pid_file" ]]; then
        pid=$(cat "$pid_file" 2>/dev/null || true)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "ERROR: node$i already running (pid $pid). Run stop.sh first." >&2
            exit 1
        else
            rm -f "$pid_file"
        fi
    fi
done

# Sanity check: required ports must be free.
ports=(5005 5006 5007 5008 5009 5010 5011 5012 5013 5014 5015 5016 \
       9090 9091 9092 9093 9094 9095 \
       51231 51232 51233 51234 51235 51236 51237 51238 51239 51241)
for port in "${ports[@]}"; do
    if ss -lnt 2>/dev/null | awk '{print $4}' | grep -q ":${port}\$"; then
        echo "ERROR: port ${port} is already in use" >&2
        exit 1
    fi
done

cd "$ROOT_DIR"

start_node() {
    local n="$1"
    local config="$LOCAL_DIR/configs/node${n}.toml"
    local log="$LOCAL_DIR/logs/node${n}.log"
    local pid_file="$LOCAL_DIR/run/node${n}.pid"
    if [[ ! -f "$config" ]]; then
        echo "ERROR: missing config $config" >&2
        exit 1
    fi
    echo "  starting node${n} (config=${config#"$ROOT_DIR"/}, log=${log#"$ROOT_DIR"/})"
    # nohup + setsid so children survive this script's shell and form their own
    # process group (so stop.sh can clean them up reliably).
    nohup setsid "$CALLD_BIN" --config "$config" >>"$log" 2>&1 &
    echo $! >"$pid_file"
}

echo "=================================================================="
echo " Callchain local devnet (6 nodes, no docker)"
echo "=================================================================="
echo "  binary       : $CALLD_BIN"
echo "  data dir     : $LOCAL_DIR/data"
echo "  log dir      : $LOCAL_DIR/logs"
echo "  pid dir      : $LOCAL_DIR/run"
echo

# Start validators first so the BFT engine has peers when full nodes connect.
for n in 1 2 3 4 5 6; do
    start_node "$n"
    # Stagger to reduce p2p connection thrash on cold start.
    sleep 0.3
done

echo
echo "All 6 nodes launched. Tail individual logs with:"
echo "    tail -f $LOCAL_DIR/logs/node1.log"
echo
echo "Check status:"
echo "    $LOCAL_DIR/scripts/status.sh"
echo
echo "Verify consensus + sync:"
echo "    $LOCAL_DIR/scripts/verify.sh"
echo
echo "Stop:"
echo "    $LOCAL_DIR/scripts/stop.sh"

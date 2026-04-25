#!/usr/bin/env bash
# Start a single-node Callchain devnet using the locally compiled binary (no Docker).
#
# The node binds to 127.0.0.1, writes data under devnet/single/data/node1/,
# and logs to devnet/single/logs/node1.log.
#
# Usage:
#   ./devnet/single/scripts/start_local.sh
#
# Environment:
#   CALLD_BIN    path to the calld binary (default: ./target/release/calld)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SINGLE_DIR="$ROOT_DIR/devnet/single"

CALLD_BIN="${CALLD_BIN:-$ROOT_DIR/target/release/calld}"

if [[ ! -x "$CALLD_BIN" ]]; then
    echo "ERROR: calld binary not found or not executable: $CALLD_BIN" >&2
    echo "Build it first with:" >&2
    echo "    cargo build --release -p call-node" >&2
    exit 1
fi

mkdir -p "$SINGLE_DIR/logs" "$SINGLE_DIR/data/node1" "$SINGLE_DIR/run"

pid_file="$SINGLE_DIR/run/node1.pid"
if [[ -f "$pid_file" ]]; then
    pid=$(cat "$pid_file" 2>/dev/null || true)
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "ERROR: single node already running (pid $pid). Run stop_local.sh first." >&2
        exit 1
    else
        rm -f "$pid_file"
    fi
fi

# Sanity check: required ports must be free.
port_in_use() {
    local p="$1"
    if command -v ss >/dev/null 2>&1; then
        ss -lnt 2>/dev/null | awk '{print $4}' | grep -q ":${p}\$"
    elif command -v netstat >/dev/null 2>&1; then
        netstat -lnt 2>/dev/null | awk '{print $4}' | grep -q ":${p}\$"
    elif command -v lsof >/dev/null 2>&1; then
        lsof -iTCP:"${p}" -sTCP:LISTEN -n -P >/dev/null 2>&1
    else
        return 1
    fi
}
ports=(5005 5006 51235 9090)
for port in "${ports[@]}"; do
    if port_in_use "$port"; then
        echo "ERROR: port ${port} is already in use" >&2
        exit 1
    fi
done

config="$SINGLE_DIR/configs/node1_local.toml"
log="$SINGLE_DIR/logs/node1.log"

if [[ ! -f "$config" ]]; then
    echo "ERROR: missing config $config" >&2
    exit 1
fi

echo "=================================================================="
echo " Callchain single-node devnet (no docker)"
echo "=================================================================="
echo "  binary       : $CALLD_BIN"
echo "  config       : ${config#"$ROOT_DIR"/}"
echo "  data dir     : $SINGLE_DIR/data"
echo "  log          : ${log#"$ROOT_DIR"/}"
echo ""

echo "  starting node1 (solo validator mode) ..."
nohup "$CALLD_BIN" --config "$config" --solo >>"$log" 2>&1 &
echo $! >"$pid_file"

echo ""
echo "Single node launched. Tail logs with:"
echo "    tail -f $SINGLE_DIR/logs/node1.log"
echo ""
echo "Stop:"
echo "    $SINGLE_DIR/scripts/stop_local.sh"

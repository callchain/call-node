#!/usr/bin/env bash
# Stop the single-node devnet started by start_local.sh.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SINGLE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

pid_file="$SINGLE_DIR/run/node1.pid"
[[ -f "$pid_file" ]] || {
    echo "No PID file found — node1 not running?"
    exit 0
}

pid=$(cat "$pid_file" 2>/dev/null || true)
if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    echo "Stopping node1 (pid $pid) ..."
    kill -TERM "$pid" 2>/dev/null || true

    # Wait up to 10s for graceful shutdown
    for _ in $(seq 1 10); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done

    if kill -0 "$pid" 2>/dev/null; then
        echo "Force-killing pid $pid"
        kill -KILL "$pid" 2>/dev/null || true
    fi
else
    echo "node1 not running (stale pid file)"
fi

rm -f "$pid_file"
echo "Stopped."

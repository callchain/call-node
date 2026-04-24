#!/usr/bin/env bash
# Wipe all local devnet state (data, logs, pid files). Refuses to run while
# any node is still up — call stop.sh first.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

for i in 1 2 3 4 5 6; do
    pid_file="$LOCAL_DIR/run/node$i.pid"
    [[ -f "$pid_file" ]] || continue
    pid=$(cat "$pid_file" 2>/dev/null || true)
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "ERROR: node$i is still running (pid $pid). Run stop.sh first." >&2
        exit 1
    fi
done

rm -rf "$LOCAL_DIR/data" "$LOCAL_DIR/logs" "$LOCAL_DIR/run"
echo "Cleaned: data/, logs/, run/ under $LOCAL_DIR"

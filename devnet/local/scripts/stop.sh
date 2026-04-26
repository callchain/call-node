#!/usr/bin/env bash
# Stop all 6 nodes started by start.sh, using their recorded PIDs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

declare -a STOPPED_PIDS=()
for i in 1 2 3 4 5 6; do
    pid_file="$LOCAL_DIR/run/node$i.pid"
    [[ -f "$pid_file" ]] || continue
    pid=$(cat "$pid_file" 2>/dev/null || true)
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "  stopping node$i (pid $pid)"
        # On Linux, `setsid` creates a new process group — kill the group first.
        # On macOS (no setsid), kill child PIDs via pkill, then the parent.
        if command -v setsid >/dev/null 2>&1; then
            kill -TERM "-${pid}" 2>/dev/null || true
        elif command -v pkill >/dev/null 2>&1; then
            pkill -TERM -P "$pid" 2>/dev/null || true
        fi
        kill -TERM "$pid" 2>/dev/null || true
        STOPPED_PIDS+=("$pid")
    else
        echo "  node$i not running (stale pid file)"
    fi
    rm -f "$pid_file"
done

# Wait up to 10s for graceful shutdown, then SIGKILL anything left.
if (( ${#STOPPED_PIDS[@]} > 0 )); then
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        alive=0
        for pid in "${STOPPED_PIDS[@]}"; do
            kill -0 "$pid" 2>/dev/null && alive=1
        done
        (( alive == 0 )) && break
        sleep 1
    done
    for pid in "${STOPPED_PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            echo "  force-killing pid $pid"
            if command -v setsid >/dev/null 2>&1; then
                kill -KILL "-${pid}" 2>/dev/null || true
            elif command -v pkill >/dev/null 2>&1; then
                pkill -KILL -P "$pid" 2>/dev/null || true
            fi
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done
fi

echo "Stopped ${#STOPPED_PIDS[@]} node(s)."

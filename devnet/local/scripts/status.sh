#!/usr/bin/env bash
# Show pid + RPC height for each local devnet node.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

NODES=(
    "node1 validator 5005"
    "node2 validator 5007"
    "node3 validator 5009"
    "node4 validator 5011"
    "node5 full      5013"
    "node6 full      5015"
)

get_height() {
    local port="$1"
    local resp
    resp=$(curl -fsS --max-time 2 -X POST "http://127.0.0.1:${port}" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' 2>/dev/null) || {
        echo ""
        return
    }
    local hex
    hex=$(printf '%s' "$resp" | sed -n 's/.*"result"[[:space:]]*:[[:space:]]*"0x\([0-9a-fA-F]*\)".*/\1/p')
    [[ -z "$hex" ]] && { echo ""; return; }
    if command -v python3 >/dev/null 2>&1; then
        python3 -c "print(int('$hex', 16))"
    else
        printf '%d\n' "0x${hex}"
    fi
}

printf '%-6s  %-9s  %-7s  %-12s  %s\n' "NODE" "ROLE" "PID" "STATUS" "HEIGHT"
echo "---------------------------------------------------------------"
for entry in "${NODES[@]}"; do
    # shellcheck disable=SC2086
    set -- $entry
    label="$1"; role="$2"; port="$3"
    pid_file="$LOCAL_DIR/run/${label}.pid"
    if [[ -f "$pid_file" ]]; then
        pid=$(cat "$pid_file" 2>/dev/null || true)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            status="running"
        else
            status="dead"
            pid="-"
        fi
    else
        status="stopped"
        pid="-"
    fi
    height=$(get_height "$port")
    [[ -z "$height" ]] && height="-"
    printf '%-6s  %-9s  %-7s  %-12s  %s\n' "$label" "$role" "$pid" "$status" "$height"
done

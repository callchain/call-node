#!/usr/bin/env bash
# Verify the LOCAL (no-docker) devnet behaves correctly:
#   1. The 4 validators reach BFT consensus and advance the SAME chain.
#   2. The 2 full nodes follow that chain (sync) instead of producing their
#      own divergent blocks.
#
# Usage:
#   ./devnet/local/scripts/verify.sh
#   OBSERVE_SECS=120 ./devnet/local/scripts/verify.sh
#
# Exit 0 on success, non-zero on failure.
set -uo pipefail

OBSERVE_SECS="${OBSERVE_SECS:-60}"
POLL_INTERVAL="${POLL_INTERVAL:-3}"
WARMUP_SECS="${WARMUP_SECS:-15}"
MIN_PROGRESS_BLOCKS="${MIN_PROGRESS_BLOCKS:-2}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# (label, role, rpc_port)
NODES=(
    "node1 validator 5005"
    "node2 validator 5007"
    "node3 validator 5009"
    "node4 validator 5011"
    "node5 full      5013"
    "node6 full      5015"
)

PASS=0
FAIL=0
ISSUES=()

color() {
    case "$1" in
        red)    printf '\033[31m%s\033[0m' "$2" ;;
        green)  printf '\033[32m%s\033[0m' "$2" ;;
        yellow) printf '\033[33m%s\033[0m' "$2" ;;
        bold)   printf '\033[1m%s\033[0m'  "$2" ;;
        *) printf '%s' "$2" ;;
    esac
}

ok()   { PASS=$((PASS+1)); echo "  $(color green '[PASS]') $*"; }
fail() { FAIL=$((FAIL+1)); ISSUES+=("$*"); echo "  $(color red   '[FAIL]') $*"; }
info() { echo "  $(color yellow '[INFO]') $*"; }

get_height() {
    local port="$1"
    local resp
    resp=$(curl -fsS --max-time 3 -X POST "http://127.0.0.1:${port}" \
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

heights_snapshot() {
    for entry in "${NODES[@]}"; do
        # shellcheck disable=SC2086
        set -- $entry
        local label="$1" role="$2" port="$3"
        local h
        h=$(get_height "$port")
        printf '%s %s %s %s\n' "$label" "$role" "$port" "${h:-NA}"
    done
}

print_snapshot() {
    local title="$1"
    local snap="$2"
    echo
    echo "  $(color bold "$title")"
    while IFS= read -r line; do
        # shellcheck disable=SC2086
        set -- $line
        printf '    %-6s %-10s rpc:%s  height=%s\n' "$1" "$2" "$3" "$4"
    done <<< "$snap"
}

is_num() { [[ "$1" =~ ^[0-9]+$ ]]; }

# Median of a list of numbers (used for the canonical validator tip).
median() {
    local sorted
    sorted=$(printf '%s\n' "$@" | sort -n)
    local n
    n=$(printf '%s\n' "$sorted" | wc -l)
    if (( n == 0 )); then
        echo 0
        return
    fi
    if (( n % 2 == 1 )); then
        printf '%s\n' "$sorted" | awk -v k=$(( (n + 1) / 2 )) 'NR==k{print; exit}'
    else
        local a b
        a=$(printf '%s\n' "$sorted" | awk -v k=$(( n / 2 )) 'NR==k{print; exit}')
        b=$(printf '%s\n' "$sorted" | awk -v k=$(( n / 2 + 1 )) 'NR==k{print; exit}')
        echo $(( (a + b) / 2 ))
    fi
}

echo "=================================================================="
echo " Callchain LOCAL devnet verification (no docker)"
echo "=================================================================="
echo "  Observation window : ${OBSERVE_SECS}s"
echo "  Poll interval      : ${POLL_INTERVAL}s"
echo "  Warmup             : ${WARMUP_SECS}s"
echo "  Min validator      : ${MIN_PROGRESS_BLOCKS} new block(s) within window"

# 1. PIDs alive
echo
color bold '[1/5] all 6 calld processes running'; echo
running=0
for entry in "${NODES[@]}"; do
    # shellcheck disable=SC2086
    set -- $entry
    label="$1"; role="$2"
    pid_file="$LOCAL_DIR/run/${label}.pid"
    if [[ -f "$pid_file" ]]; then
        pid=$(cat "$pid_file" 2>/dev/null || true)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            ok "${label} (${role}) running (pid ${pid})"
            running=$((running + 1))
        else
            fail "${label} (${role}) pid file exists but process is dead (pid ${pid:-?})"
        fi
    else
        fail "${label} (${role}) not started — no pid file at ${pid_file}"
    fi
done
if (( running != 6 )); then
    info "only ${running}/6 nodes running; remaining checks may be inconclusive"
fi

# 2. RPC reachable
echo
color bold '[2/5] RPC reachable on every node'; echo
for entry in "${NODES[@]}"; do
    # shellcheck disable=SC2086
    set -- $entry
    label="$1"; role="$2"; port="$3"
    h=$(get_height "$port")
    if [[ -z "$h" ]]; then
        fail "${label} (${role}) RPC :${port} not reachable / no eth_blockNumber"
    else
        ok "${label} (${role}) RPC :${port} → height ${h}"
    fi
done

info "warming up for ${WARMUP_SECS}s before measuring progress…"
sleep "$WARMUP_SECS"

START_SNAP=$(heights_snapshot)
print_snapshot "Initial snapshot:" "$START_SNAP"

elapsed=0
while [[ "$elapsed" -lt "$OBSERVE_SECS" ]]; do
    sleep "$POLL_INTERVAL"
    elapsed=$((elapsed + POLL_INTERVAL))
done

END_SNAP=$(heights_snapshot)
print_snapshot "Snapshot after ${OBSERVE_SECS}s:" "$END_SNAP"

declare -A START END
while IFS= read -r line; do
    # shellcheck disable=SC2086
    set -- $line
    START["$1"]=$4
done <<< "$START_SNAP"
while IFS= read -r line; do
    # shellcheck disable=SC2086
    set -- $line
    END["$1"]=$4
done <<< "$END_SNAP"

# 3. Validator consensus: advance & agree
echo
color bold '[3/5] Validators reach consensus (advance & agree)'; echo
val_heights=()
for label in node1 node2 node3 node4; do
    s="${START[$label]:-NA}"
    e="${END[$label]:-NA}"
    if ! is_num "$e"; then
        fail "${label}: end height not numeric (${e})"
        continue
    fi
    if is_num "$s"; then
        delta=$((e - s))
        if [[ "$delta" -lt "$MIN_PROGRESS_BLOCKS" ]]; then
            fail "${label}: only advanced ${delta} block(s) in ${OBSERVE_SECS}s (need ≥ ${MIN_PROGRESS_BLOCKS}); validators are not finalising blocks"
        else
            ok "${label}: advanced ${delta} block(s) (${s} → ${e})"
        fi
    fi
    val_heights+=("$e")
done

if [[ "${#val_heights[@]}" -ge 2 ]]; then
    min=${val_heights[0]}; max=${val_heights[0]}
    for h in "${val_heights[@]}"; do
        is_num "$h" || continue
        (( h < min )) && min=$h
        (( h > max )) && max=$h
    done
    spread=$((max - min))
    if [[ "$spread" -le 2 ]]; then
        ok "validators agree on chain tip (heights ${val_heights[*]}; spread=${spread})"
    else
        fail "validator heights diverge — spread=${spread}, heights=${val_heights[*]}; this looks like separate chains, not consensus"
    fi
fi

# 4. Full nodes follow validators
echo
color bold '[4/5] Full nodes sync from validators (no local production)'; echo

# Canonical reference: median of numeric validator heights.
numeric_vals=()
for h in "${val_heights[@]}"; do
    is_num "$h" && numeric_vals+=("$h")
done
if (( ${#numeric_vals[@]} == 0 )); then
    canon=0
    info "no numeric validator heights — using 0 as canonical reference"
else
    canon=$(median "${numeric_vals[@]}")
fi

for label in node5 node6; do
    s="${START[$label]:-NA}"
    e="${END[$label]:-NA}"
    if ! is_num "$e"; then
        fail "${label}: end height not numeric (${e})"
        continue
    fi

    if is_num "$s"; then
        delta=$((e - s))
        if [[ "$delta" -lt 1 && "$canon" -gt "$s" ]]; then
            fail "${label}: did not advance (${s} → ${e}) while validators reached ${canon}; sync is broken"
        fi
    fi

    if is_num "$canon" && (( e > canon + 1 )); then
        fail "${label}: height ${e} is ahead of validator tip ${canon} by $((e - canon)) — full node is producing local blocks, not syncing"
        continue
    fi

    drift=$((canon - e))
    (( drift < 0 )) && drift=$((-drift))
    if [[ "$drift" -le 5 ]]; then
        ok "${label}: synced (height=${e}, validator tip=${canon}, drift=${drift})"
    else
        fail "${label}: lagging — height=${e}, validator tip=${canon}, drift=${drift} blocks"
    fi
done

# 5. Cross-check: all 6 converge
echo
color bold '[5/5] All 6 nodes converge on the same chain'; echo
all_heights=()
for label in node1 node2 node3 node4 node5 node6; do
    h="${END[$label]:-NA}"
    is_num "$h" && all_heights+=("$h")
done
if [[ "${#all_heights[@]}" -ge 2 ]]; then
    min=${all_heights[0]}; max=${all_heights[0]}
    for h in "${all_heights[@]}"; do
        (( h < min )) && min=$h
        (( h > max )) && max=$h
    done
    spread=$((max - min))
    if [[ "$spread" -le 5 ]]; then
        ok "all 6 nodes agree (min=${min}, max=${max}, spread=${spread})"
    else
        fail "chain tips diverge across the 6 nodes (min=${min}, max=${max}, spread=${spread})"
    fi
fi

echo
echo "=================================================================="
echo " Verification result: $(color green "${PASS} passed"), $(color red "${FAIL} failed")"
echo "=================================================================="
if [[ "$FAIL" -gt 0 ]]; then
    echo
    echo "Failures:"
    for issue in "${ISSUES[@]}"; do
        echo "  - $issue"
    done
    exit 1
fi
exit 0

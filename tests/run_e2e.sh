#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Parse arguments ──
USE_LOCAL=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --local)
            USE_LOCAL=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--local]"
            echo ""
            echo "  (default)   Run E2E tests against docker-compose devnet"
            echo "  --local     Run E2E tests against local native devnet (target/release/calld)"
            echo ""
            echo "Local mode is faster for iteration:"
            echo "  cargo build --release -p call-node"
            echo "  $0 --local"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--local]"
            exit 1
            ;;
    esac
done

# ── Determine devnet backend ──
if $USE_LOCAL; then
    CALLD_BIN="${CALLD_BIN:-$PROJECT_ROOT/target/release/calld}"
    LOCAL_SCRIPTS="$PROJECT_ROOT/devnet/local/scripts"
    LOCAL_DIR="$PROJECT_ROOT/devnet/local"
else
    if command -v docker-compose &>/dev/null; then
        COMPOSE="docker-compose"
    elif docker compose version &>/dev/null; then
        COMPOSE="docker compose"
    else
        echo "ERROR: docker compose not found"
        exit 1
    fi
fi

echo "============================================"
if $USE_LOCAL; then
    echo "Callchain Devnet E2E — Local Native Mode"
else
    echo "Callchain Devnet E2E — Docker Mode"
fi
echo "============================================"
echo

# ── Clear old devnet data ──
echo "[1/5] Clearing old devnet data..."
cd "$PROJECT_ROOT"
if $USE_LOCAL; then
    "$LOCAL_SCRIPTS/stop.sh" 2>/dev/null || true
    "$LOCAL_SCRIPTS/clean.sh" 2>/dev/null || true
else
    $COMPOSE -f devnet/docker-compose.yml down -v --remove-orphans 2>/dev/null || true
fi
echo

# ── Start devnet ──
echo "[2/5] Starting devnet..."
cd "$PROJECT_ROOT"
if $USE_LOCAL; then
    if [[ ! -x "$CALLD_BIN" ]]; then
        echo "ERROR: calld binary not found: $CALLD_BIN"
        echo "Build it first: cargo build --release -p call-node"
        exit 1
    fi
    "$LOCAL_SCRIPTS/start.sh"
else
    $COMPOSE -f devnet/docker-compose.yml up -d --build
fi
echo

# ── Wait for nodes ──
echo "[3/5] Waiting for nodes to be ready..."
for port in 5005 5007 5009 5011 5013 5015; do
    for i in {1..60}; do
        if curl -s --noproxy "*" -X POST "http://127.0.0.1:$port" \
            -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
            | grep -q '"result"'; then
            echo "  node on :$port ready"
            break
        fi
        sleep 1
        if [ $i -eq 60 ]; then
            echo "  ERROR: node on :$port did not start in 60s"
            exit 1
        fi
    done
done
echo

# ── Run tests ──
echo "[4/5] Running E2E tests..."
cd "$SCRIPT_DIR"

# Clean shared nonce state before a fresh devnet run
rm -f .nonce_state.json

BASIC_OK=true
STRESS_OK=true
TX_OK=true
VAL_OK=true

python3 test_basic.py || BASIC_OK=false
rm -f .nonce_state.json
python3 test_stress.py || STRESS_OK=false
rm -f .nonce_state.json
python3 test_transactions.py || TX_OK=false
rm -f .nonce_state.json
python3 test_validator.py || VAL_OK=false

# ── Report ──
echo
echo "============================================"
if $BASIC_OK && $STRESS_OK && $TX_OK && $VAL_OK; then
    echo "All tests PASSED"
    EXIT_CODE=0
else
    echo "Some tests FAILED"
    EXIT_CODE=1
fi
echo "============================================"

# ── Ask to stop devnet ──
echo
read -p "Stop devnet? [Y/n] " ans
if [[ -z "$ans" || "$ans" =~ ^[Yy]$ ]]; then
    echo "[5/5] Stopping devnet..."
    cd "$PROJECT_ROOT"
    if $USE_LOCAL; then
        "$LOCAL_SCRIPTS/stop.sh"
    else
        $COMPOSE -f devnet/docker-compose.yml down
    fi
    echo "Done."
else
    echo "[5/5] Devnet left running."
fi

exit $EXIT_CODE

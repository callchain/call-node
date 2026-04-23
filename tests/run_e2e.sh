#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "============================================"
echo "Callchain Devnet E2E — 6-Node Validator Network"
echo "============================================"
echo

# ── Check docker compose ──
if command -v docker-compose &>/dev/null; then
    COMPOSE="docker-compose"
elif docker compose version &>/dev/null; then
    COMPOSE="docker compose"
else
    echo "ERROR: docker compose not found"
    exit 1
fi

# ── Clear old devnet data ──
echo "[1/5] Clearing old devnet data..."
cd "$PROJECT_ROOT"
$COMPOSE -f devnet/docker-compose.yml down -v --remove-orphans 2>/dev/null || true
echo

# ── Start devnet ──
echo "[2/5] Starting devnet..."
cd "$PROJECT_ROOT"
$COMPOSE -f devnet/docker-compose.yml up -d --build
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
BASIC_OK=true
STRESS_OK=true

TX_OK=true

python3 test_basic.py || BASIC_OK=false
python3 test_stress.py || STRESS_OK=false
python3 test_transactions.py || TX_OK=false

# ── Report ──
echo
echo "============================================"
if $BASIC_OK && $STRESS_OK && $TX_OK; then
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
    $COMPOSE -f devnet/docker-compose.yml down
    echo "Done."
else
    echo "[5/5] Devnet left running."
fi

exit $EXIT_CODE

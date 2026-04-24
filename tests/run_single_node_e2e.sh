#!/usr/bin/env bash
# Run live single-node devnet E2E tests (Docker + Python RPC tests)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "============================================"
echo "Callchain Single-Node Devnet E2E Tests"
echo "============================================"
echo ""

# ── Check docker compose ──
if command -v docker-compose &>/dev/null; then
  COMPOSE="docker-compose"
elif docker compose version &>/dev/null; then
  COMPOSE="docker compose"
else
  echo "ERROR: docker compose not found"
  exit 1
fi

# ── Clear old data ──
echo "[1/5] Clearing old single-node data..."
cd "$PROJECT_ROOT"
$COMPOSE -f devnet/single/docker-compose.yml down -v --remove-orphans 2>/dev/null || true
echo ""

# ── Start single-node devnet ──
echo "[2/5] Starting single-node devnet..."
cd "$PROJECT_ROOT"
$COMPOSE -f devnet/single/docker-compose.yml up -d --build
echo ""

# ── Wait for node readiness ──
echo "[3/5] Waiting for node to be ready (RPC :5005)..."
for i in {1..60}; do
  if curl -s --noproxy "*" -X POST "http://127.0.0.1:5005" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    | grep -q '"result"'; then
    echo "  Node ready on :5005"
    break
  fi
  sleep 1
  if [ "$i" -eq 60 ]; then
    echo "  ERROR: node did not start in 60s"
    exit 1
  fi
done
echo ""

# ── Run Python E2E tests ──
echo "[4/5] Running Python E2E tests..."
cd "$SCRIPT_DIR"

# Clean shared nonce state before a fresh devnet run
rm -f .nonce_state.json

export CALLCHAIN_SINGLE_NODE=1

BASIC_OK=true
STRESS_OK=true
TX_OK=true

python3 test_basic.py || BASIC_OK=false
rm -f .nonce_state.json
python3 test_stress.py || STRESS_OK=false
rm -f .nonce_state.json
python3 test_transactions.py || TX_OK=false

echo ""
echo "============================================"
if $BASIC_OK && $STRESS_OK && $TX_OK; then
  echo "All single-node E2E tests PASSED"
  EXIT_CODE=0
else
  echo "Some single-node E2E tests FAILED"
  EXIT_CODE=1
fi
echo "============================================"

# ── Stop devnet ──
echo ""
read -p "Stop single-node devnet? [Y/n] " ans
if [[ -z "$ans" || "$ans" =~ ^[Yy]$ ]]; then
  echo "[5/5] Stopping single-node devnet..."
  cd "$PROJECT_ROOT"
  $COMPOSE -f devnet/single/docker-compose.yml down
  echo "Done."
else
  echo "[5/5] Single-node devnet left running."
fi

exit $EXIT_CODE

#!/usr/bin/env bash
# Run live single-node devnet E2E tests WITHOUT Docker (local binary + Python RPC tests)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CALLD_BIN="${CALLD_BIN:-$PROJECT_ROOT/target/release/calld}"

if [[ ! -x "$CALLD_BIN" ]]; then
    echo "ERROR: calld binary not found: $CALLD_BIN" >&2
    echo "Build it first: cargo build --release -p call-node" >&2
    exit 1
fi

echo "============================================"
echo "Callchain Single-Node Local E2E Tests"
echo "============================================"
echo ""

# ── Clear old data ──
echo "[1/5] Clearing old single-node data ..."
cd "$PROJECT_ROOT"
rm -rf devnet/single/data/node1 devnet/single/logs devnet/single/run devnet/single/*.json 2>/dev/null || true

# ── Ensure genesis exists ──
if [[ ! -f "devnet/genesis.json" ]]; then
    echo "ERROR: devnet/genesis.json not found" >&2
    exit 1
fi

# ── Start single-node devnet ──
echo "[2/5] Starting single-node devnet (no docker)..."
CALLD_BIN="$CALLD_BIN" "$PROJECT_ROOT/devnet/single/scripts/start_local.sh"
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
    "$PROJECT_ROOT/devnet/single/scripts/stop_local.sh" || true
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
echo "[5/5] Stopping single-node devnet ..."
"$PROJECT_ROOT/devnet/single/scripts/stop_local.sh"
echo "Done."

exit $EXIT_CODE

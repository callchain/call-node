#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "============================================"
echo "Callchain Devnet E2E Test Suite"
echo "============================================"
echo

# ── Check devnet is reachable ──
if ! python3 -c "
import sys
sys.path.insert(0, '.')
from rpc_client import CallchainNode
node = CallchainNode('http://127.0.0.1:5005')
try:
    h = node.block_number()
    print(f'Devnet node1 at height {h}')
except Exception as e:
    print(f'ERROR: Cannot reach devnet node1 at 127.0.0.1:5005 — {e}')
    print('Start the devnet first with: ./devnet/scripts/start.sh')
    sys.exit(1)
" 2>/dev/null; then
    echo "ERROR: Devnet is not running. Start it first with:"
    echo "    ./devnet/scripts/start.sh"
    exit 1
fi

echo

# ── Run basic tests ──
python3 test_basic.py
BASIC_EXIT=$?

# ── Run stress tests ──
python3 test_stress.py
STRESS_EXIT=$?

# ── Run transaction type tests ──
python3 test_transactions.py
TX_EXIT=$?

# ── Summary ──
echo
echo "============================================"
if [ $BASIC_EXIT -eq 0 ] && [ $STRESS_EXIT -eq 0 ] && [ $TX_EXIT -eq 0 ]; then
    echo "All tests passed."
    exit 0
else
    echo "Some tests failed."
    exit 1
fi

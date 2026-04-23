#!/usr/bin/env bash
# Run network integration tests (real commonware-p2p with localhost TCP sockets)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "============================================"
echo "Callchain Network Integration Tests"
echo "============================================"
echo ""

cd "$PROJECT_ROOT"

echo "[RUN] cargo test -p call-network --test integration_test"
cargo test -p call-network --test integration_test -- --nocapture

echo ""
echo "============================================"
echo "Network integration tests PASSED"
echo "============================================"

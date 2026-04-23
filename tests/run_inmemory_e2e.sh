#!/usr/bin/env bash
# Run in-memory Rust E2E tests (no Docker, deterministic TestNode harness)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "============================================"
echo "Callchain In-Memory E2E Tests"
echo "============================================"
echo ""

cd "$PROJECT_ROOT"

TESTS=(
  "test_governance_e2e"
  "test_shielded_e2e"
  "test_bridge_e2e"
  "test_light_client_e2e"
  "test_websocket_e2e"
)

ALL_OK=true

for test_name in "${TESTS[@]}"; do
  echo "[RUN] $test_name ..."
  if cargo test -p call-node --test "$test_name" -- --nocapture; then
    echo "[PASS] $test_name"
  else
    echo "[FAIL] $test_name"
    ALL_OK=false
  fi
  echo ""
done

echo "============================================"
if $ALL_OK; then
  echo "All in-memory E2E tests PASSED"
  exit 0
else
  echo "Some in-memory E2E tests FAILED"
  exit 1
fi

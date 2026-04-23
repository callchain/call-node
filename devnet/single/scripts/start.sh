#!/usr/bin/env bash
# Start the single-node devnet
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Starting Callchain single-node devnet ==="
docker compose -f devnet/single/docker-compose.yml up -d

echo ""
echo "=== Single node started ==="
echo "RPC:    http://127.0.0.1:5005"
echo "WS:     ws://127.0.0.1:5006"
echo "P2P:    :51235"
echo "Metrics: :9090"
echo ""
echo "Stop:  ./devnet/single/scripts/stop.sh"

#!/usr/bin/env bash
# Start the devnet (all 4 nodes)
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Starting Callchain devnet ==="
docker compose -f devnet/docker-compose.yml up -d --build

echo ""
echo "=== Devnet started ==="
echo "Node 1: RPC http://127.0.0.1:5005  P2P :51235  Metrics :9090"
echo "Node 2: RPC http://127.0.0.1:5007  P2P :51236  Metrics :9091"
echo "Node 3: RPC http://127.0.0.1:5009  P2P :51237  Metrics :9092"
echo "Node 4: RPC http://127.0.0.1:5011  P2P :51238  Metrics :9093"
echo ""
echo "Logs:  ./devnet/scripts/logs.sh"
echo "Status: ./devnet/scripts/status.sh"
echo "Stop:  ./devnet/scripts/stop.sh"

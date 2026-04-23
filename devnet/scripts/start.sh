#!/usr/bin/env bash
# Start the devnet (all 6 nodes)
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Starting Callchain devnet ==="
docker compose -f devnet/docker-compose.yml up -d

echo ""
echo "=== Devnet started ==="
echo "Validators:"
echo "  Node 1: RPC http://127.0.0.1:5005  WS :5006  P2P :51235  Metrics :9090"
echo "  Node 2: RPC http://127.0.0.1:5007  WS :5008  P2P :51236  Metrics :9091"
echo "  Node 3: RPC http://127.0.0.1:5009  WS :5010  P2P :51237  Metrics :9092"
echo "  Node 4: RPC http://127.0.0.1:5011  WS :5012  P2P :51238  Metrics :9093"
echo "Full nodes:"
echo "  Node 5: RPC http://127.0.0.1:5013  WS :5014  P2P :51239  Metrics :9094"
echo "  Node 6: RPC http://127.0.0.1:5015  WS :5016  P2P :51240  Metrics :9095"
echo ""
echo "Logs:  ./devnet/scripts/logs.sh"
echo "Status: ./devnet/scripts/status.sh"
echo "Stop:  ./devnet/scripts/stop.sh"

#!/usr/bin/env bash
# Query devnet node info via RPC
# Usage: query.sh [1|2|3|4] [method]
set -euo pipefail

NODE="${1:-1}"
METHOD="${2:-call_serverInfo}"

case "$NODE" in
    1) PORT=5005 ;;
    2) PORT=5007 ;;
    3) PORT=5009 ;;
    4) PORT=5011 ;;
    *) echo "Invalid node: $NODE (use 1-4)"; exit 1 ;;
esac

echo "=== Querying node $NODE (port $PORT) ==="
curl -s -X POST "http://127.0.0.1:$PORT" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${METHOD}\",\"params\":[]}" \
    | python3 -m json.tool 2>/dev/null || curl -s -X POST "http://127.0.0.1:$PORT" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${METHOD}\",\"params\":[]}"

echo ""

#!/usr/bin/env bash
set -euo pipefail

CHAIN_ID=1337
GENESIS_FILE="example/genesis.example.json"
DATA_DIR="/tmp/callchain-devnet"

echo "=== Callchain Devnet Setup ==="
echo ""

case "${1:-help}" in
  init)
    echo "Initializing devnet node..."
    mkdir -p "$DATA_DIR"
    echo "Data directory: $DATA_DIR"
    if [ -f "$GENESIS_FILE" ]; then
      cp "$GENESIS_FILE" "$DATA_DIR/genesis.json"
      echo "Genesis copied to $DATA_DIR/genesis.json"
    fi
    echo "Run: ./target/release/calld --data-dir $DATA_DIR --genesis-path $DATA_DIR/genesis.json"
    ;;

  start)
    echo "Starting devnet node..."
    ./target/release/calld \
      --data-dir "$DATA_DIR" \
      --genesis-path "$DATA_DIR/genesis.json" \
      --p2p-listen-addr 0.0.0.0:51235 \
      --http-addr 127.0.0.1:5005 \
      --ws-addr 127.0.0.1:5006 \
      --log-level info &
    echo "Node started (PID: $!)"
    echo "RPC: http://127.0.0.1:5005"
    echo "WS:  ws://127.0.0.1:5006"
    ;;

  stop)
    echo "Stopping devnet node..."
    pkill -f "calld.*$DATA_DIR" || true
    echo "Node stopped"
    ;;

  status)
    echo "Checking node status..."
    curl -s -X POST http://127.0.0.1:5005 \
      -H "Content-Type: application/json" \
      -d '{"jsonrpc":"2.0","method":"server_info","id":1}' | \
      python3 -m json.tool 2>/dev/null || echo "Node not running"
    ;;

  clean)
    echo "Removing devnet data..."
    rm -rf "$DATA_DIR"
    echo "Cleaned"
    ;;

  *)
    echo "Usage: $0 {init|start|stop|status|clean}"
    echo ""
    echo "Commands:"
    echo "  init    Initialize devnet data directory"
    echo "  start   Start a devnet node in background"
    echo "  stop    Stop the running devnet node"
    echo "  status  Check node health via RPC"
    echo "  clean   Remove all devnet data"
    ;;
esac

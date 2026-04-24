#!/usr/bin/env bash
# Show logs for devnet nodes
# Usage: logs.sh [node1|node2|node3|node4|node5|node6] [--follow]
set -euo pipefail

cd "$(dirname "$0")/.."

SERVICE="${1:-}"
FOLLOW="${2:-}"
ARGS=""

if [[ -n "$SERVICE" ]]; then
    ARGS="$SERVICE"
fi
if [[ "$FOLLOW" == "--follow" || "$FOLLOW" == "-f" ]]; then
    ARGS="$ARGS -f"
fi

if [[ -z "$SERVICE" ]]; then
    echo "=== Callchain Devnet Logs (all nodes) ==="
    echo "Tip: run '$0 node1 -f' to follow a single node"
fi

docker compose -f docker-compose.yml logs $ARGS

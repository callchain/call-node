#!/usr/bin/env bash
# Stop the single-node devnet
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Stopping Callchain single-node devnet ==="
docker compose -f devnet/single/docker-compose.yml down

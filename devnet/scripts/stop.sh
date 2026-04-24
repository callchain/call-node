#!/usr/bin/env bash
# Stop the devnet
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Stopping Callchain devnet ==="
docker compose -f docker-compose.yml down

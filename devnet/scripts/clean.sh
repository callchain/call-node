#!/usr/bin/env bash
# Reset the devnet — stop all nodes and delete data volumes
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Resetting Callchain devnet ==="
docker compose -f devnet/docker-compose.yml down -v
echo "=== All data volumes removed ==="

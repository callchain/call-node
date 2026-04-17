#!/usr/bin/env bash
# Show devnet node status
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Callchain Devnet Status ==="
docker compose -f devnet/docker-compose.yml ps

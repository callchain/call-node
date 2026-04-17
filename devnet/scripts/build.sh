#!/usr/bin/env bash
# Build the Docker image for the devnet
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Building Callchain devnet image ==="
docker build -t callchain-devnet -f Dockerfile .
echo "=== Image built: callchain-devnet ==="

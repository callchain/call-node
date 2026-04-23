#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

IMAGE_NAME="callchain/calld"
IMAGE_TAG="latest"
DOCKERFILE="${DOCKERFILE:-$PROJECT_ROOT/Dockerfile}"

echo "============================================"
echo "Build calld Docker Image"
echo "============================================"
echo
echo "  Image : $IMAGE_NAME:$IMAGE_TAG"
echo "  Dockerfile: $DOCKERFILE"
echo

cd "$PROJECT_ROOT"

docker build \
    -f "$DOCKERFILE" \
    -t "$IMAGE_NAME:$IMAGE_TAG" \
    --no-cache \
    --build-arg BUILDKIT_INLINE_CACHE=1 \
    "$PROJECT_ROOT"

echo
echo "============================================"
echo "Built: $IMAGE_NAME:$IMAGE_TAG"
echo "============================================"

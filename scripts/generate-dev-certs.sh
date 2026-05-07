#!/bin/bash
# Generate self-signed certificates for devnet RPC TLS testing.
# Usage: ./scripts/generate-dev-certs.sh [output_dir]
# Default output: ./data/certs/

set -euo pipefail

CERT_DIR="${1:-./data/certs}"
mkdir -p "$CERT_DIR"

CERT_FILE="$CERT_DIR/node.crt"
KEY_FILE="$CERT_DIR/node.key"

if [ -f "$CERT_FILE" ] && [ -f "$KEY_FILE" ]; then
    echo "Certificates already exist:"
    echo "  $CERT_FILE"
    echo "  $KEY_FILE"
    echo "Remove them to regenerate."
    exit 0
fi

echo "Generating self-signed certificate for devnet..."

# Use OpenSSL to generate a P-256 key + self-signed cert valid for 365 days
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    -keyout "$KEY_FILE" -out "$CERT_FILE" \
    -days 365 -nodes \
    -subj "/CN=localhost/O=Callchain Devnet" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

chmod 600 "$KEY_FILE"

echo "Done."
echo "  Certificate: $CERT_FILE"
echo "  Private key: $KEY_FILE"
echo ""
echo "Add to your node config:"
echo "  [rpc]"
echo "  tls_cert = \"$CERT_FILE\""
echo "  tls_key  = \"$KEY_FILE\""

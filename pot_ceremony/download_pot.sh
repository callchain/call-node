#!/usr/bin/env bash
# Option A: Download the Perpetual Powers of Tau (already completed ceremony)
# This is the RECOMMENDED approach — no need to run your own ceremony.
#
# The Perpetual Powers of Tau has 1000+ unique participants and covers
# up to 2^28 (~268M) constraints — more than enough for all three circuits:
#   - Deposit:    ~350 constraints
#   - Withdraw:   ~3,551 constraints
#   - Transfer:   ~7,800 constraints
#
# Usage: ./download_pot.sh [POT_SIZE]
#   POT_SIZE defaults to 25 (2^25 = 33M constraints, ~2GB download)
#   Use 28 for the full 2^28 (~128GB) if you want maximum headroom.

set -euo pipefail

POT_SIZE="${1:-25}"
OUTPUT_DIR="./pot_transcript"
mkdir -p "$OUTPUT_DIR"

echo "=== Download Perpetual Powers of Tau ==="
echo "Circuit size: 2^$POT_SIZE constraints"
echo ""

# The Perpetual Powers of Tau final transcripts are hosted publicly.
# Official source: https://github.com/privacy-scaling-explorations/perpetualpowersoftau

case "$POT_SIZE" in
    22)
        # 2^22 = 4M constraints, ~256MB
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0022/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    23)
        # 2^23 = 8M constraints, ~512MB
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0023/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    24)
        # 2^24 = 16M constraints, ~1GB
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0024/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    25)
        # 2^25 = 33M constraints, ~2GB (recommended for Callchain)
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0025/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    26)
        # 2^26 = 67M constraints, ~4GB
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0026/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    27)
        # 2^27 = 134M constraints, ~8GB
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0027/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    28)
        # 2^28 = 268M constraints, ~128GB (maximum)
        URL="https://ppot.s3.eu-central-1.amazonaws.com/0028/powersOfTau28_hez_final_${POT_SIZE}.ptau"
        ;;
    *)
        echo "ERROR: POT_SIZE must be between 22 and 28"
        echo "For Callchain circuits (~7,800 constraints max), 25 is sufficient."
        exit 1
        ;;
esac

OUTPUT_FILE="$OUTPUT_DIR/pot_final.ptau"

if [ -f "$OUTPUT_FILE" ]; then
    echo "File already exists at $OUTPUT_FILE"
    echo "Delete it first if you want to re-download."
    exit 1
fi

echo "Downloading from: $URL"
echo "Output: $OUTPUT_FILE"
echo "This may take a while depending on your connection..."
echo ""

curl -L "$URL" -o "$OUTPUT_FILE"

echo ""
echo "Download complete. Verifying transcript..."

# Verify the downloaded transcript against known checksums
snarkjs powersoftau verify "$OUTPUT_FILE" || {
    echo "WARNING: Transcript verification failed!"
    echo "The download may be corrupted. Try again."
    exit 1
}

echo ""
echo "=== Perpetual Powers of Tau Ready ==="
echo "File: $OUTPUT_FILE"
du -h "$OUTPUT_FILE"
echo ""
echo "Next step: ./phase2_derive.sh $OUTPUT_FILE"

#!/usr/bin/env bash
# Phase 2: Derive circuit-specific Groth16 keys from the universal CRS
# This step has NO trust assumptions — anyone can run it and get the same keys.
#
# Prerequisites:
#   - A final universal CRS (pot_final.ptau) from run_ceremony.sh or download_pot.sh
#   - R1CS constraint files for each circuit (exported from arkworks)
#   - snarkjs installed: npm install -g snarkjs
#
# Usage: ./phase2_derive.sh [POT_PATH]
#   POT_PATH defaults to ./pot_final.ptau

set -euo pipefail

POT_PATH="${1:-./pot_final.ptau}"
OUTPUT_DIR="./circuit_keys"
mkdir -p "$OUTPUT_DIR"

CIRCUITS=("deposit" "transfer" "withdraw")

if [ ! -f "$POT_PATH" ]; then
    echo "ERROR: Universal CRS not found at $POT_PATH"
    echo "Run download_pot.sh or run_ceremony.sh first."
    exit 1
fi

echo "=== Phase 2: Circuit-Specific Key Derivation ==="
echo "Universal CRS: $POT_PATH"
echo "Output directory: $OUTPUT_DIR"
echo ""

for CIRCUIT in "${CIRCUITS[@]}"; do
    R1CS_FILE="./r1cs/${CIRCUIT}.r1cs"
    if [ ! -f "$R1CS_FILE" ]; then
        echo "WARNING: R1CS file not found: $R1CS_FILE"
        echo "  Skipping $CIRCUIT circuit. Export R1CS from arkworks first."
        echo "  See export_r1cs.rs for the export tool."
        echo ""
        continue
    fi

    echo "--- $CIRCUIT circuit ---"

    # Step 1: Initial setup (derives zkey from universal CRS + R1CS)
    echo "  [1/3] Groth16 setup..."
    snarkjs groth16 setup \
        "$R1CS_FILE" \
        "$POT_PATH" \
        "$OUTPUT_DIR/${CIRCUIT}_0000.zkey"

    # Step 2: Contribute (anyone can do this, no trust assumption)
    echo "  [2/3] Key contribution..."
    snarkjs zkey contribute \
        "$OUTPUT_DIR/${CIRCUIT}_0000.zkey" \
        "$OUTPUT_DIR/${CIRCUIT}_final.zkey" \
        --name="Callchain $CIRCUIT circuit contribution" \
        -e="phase2_contribution"

    # Step 3: Export verifying key
    echo "  [3/3] Exporting verifying key..."
    snarkjs zkey export verificationkey \
        "$OUTPUT_DIR/${CIRCUIT}_final.zkey" \
        "$OUTPUT_DIR/${CIRCUIT}_vk.json"

    # Export Solidity verifier contract (for EVM on-chain verification)
    echo "  Exporting Solidity verifier..."
    snarkjs zkey export solidityverifier \
        "$OUTPUT_DIR/${CIRCUIT}_final.zkey" \
        "$OUTPUT_DIR/Verifier_${CIRCUIT}.sol"

    # Compute VK hash for genesis embedding
    VK_HASH=$(sha256sum "$OUTPUT_DIR/${CIRCUIT}_vk.json" | awk '{print $1}')
    echo "  VK hash (for genesis): $VK_HASH"

    # Clean up intermediate files
    rm -f "$OUTPUT_DIR/${CIRCUIT}_0000.zkey"

    echo ""
done

echo "=== Phase 2 Complete ==="
echo ""
echo "Generated files in $OUTPUT_DIR/:"
ls -lh "$OUTPUT_DIR/"
echo ""
echo "Next steps:"
echo "  1. Copy *_vk.json to crates/shielded/keys/"
echo "  2. Embed VK hashes in genesis config"
echo "  3. Load VKs at node startup via RealProver::load()"

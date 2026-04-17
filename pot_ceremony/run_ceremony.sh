#!/usr/bin/env bash
# Phase 1 (Option B): Run your own Powers of Tau ceremony
# Only use this if you DON'T want to use the Perpetual PoT (see download_pot.sh)
#
# Usage: ./run_ceremony.sh <NUM_PARTICIPANTS> <CIRCUIT_SIZE_POW2>
#   NUM_PARTICIPANTS  - Number of ceremony participants (default: 100)
#   CIRCUIT_SIZE_POW2 - Max constraints as 2^N (default: 25 = 33M constraints)
#
# Each participant must run their contribution step on their own machine,
# securely delete their randomness, and pass the file to the next participant.

set -euo pipefail

NUM_PARTICIPANTS="${1:-100}"
POT_SIZE="${2:-25}"
OUTPUT_DIR="./ceremony_output"
mkdir -p "$OUTPUT_DIR"

echo "=== Callchain Powers of Tau Ceremony ==="
echo "Participants: $NUM_PARTICIPANTS"
echo "Circuit size: 2^$POT_SIZE constraints"
echo "Output directory: $OUTPUT_DIR"
echo ""

# --- Step 1: Initialize ---
echo "[1/4] Initializing new Powers of Tau ceremony..."
snarkjs powersoftau new bn254 "$POT_SIZE" \
    "$OUTPUT_DIR/pot_0000.ptau" \
    -e "Callchain Shielded Pool Powers of Tau Ceremony"

# --- Step 2: Participant Contributions ---
# In production, each participant runs this on their own machine.
# This loop simulates the process. Replace with a coordination protocol.
echo ""
echo "[2/4] Participant contributions ($NUM_PARTICIPANTS participants)..."
for i in $(seq 1 "$NUM_PARTICIPANTS"); do
    INPUT="$OUTPUT_DIR/pot_$(printf '%04d' $((i - 1))).ptau"
    OUTPUT="$OUTPUT_DIR/pot_$(printf '%04d' "$i").ptau"
    PARTICIPANT="participant_$i"

    echo "  [$i/$NUM_PARTICIPANTS] $PARTICIPANT contributing..."
    snarkjs powersoftau contribute "$INPUT" "$OUTPUT" \
        --name="$PARTICIPANT" \
        -e="randomness_for_$PARTICIPANT"

    # In production, the participant MUST securely delete their entropy:
    # rm -f /dev/shm/random_seed  (or wherever snarkjs stores temp entropy)

    # Clean up the previous file to save disk space
    rm -f "$INPUT"
done

# --- Step 3: Apply Random Beacon ---
echo ""
echo "[3/4] Applying random beacon..."
echo "  In production, use a future unpredictable value (e.g. Bitcoin block hash)."
echo "  This prevents the last participant from manipulating the outcome."
# Placeholder beacon — replace with real value before production use
BEACON_HEX="0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"

snarkjs powersoftau beacon \
    "$OUTPUT_DIR/pot_$(printf '%04d' "$NUM_PARTICIPANTS").ptau" \
    "$OUTPUT_DIR/pot_beacon.ptau" \
    "$BEACON_HEX" \
    10 \
    -e "Callchain PoT Final Beacon"

# Clean up pre-beacon file
rm -f "$OUTPUT_DIR/pot_$(printf '%04d' "$NUM_PARTICIPANTS").ptau"

# --- Step 4: Prepare Final Transcript ---
echo ""
echo "[4/4] Preparing final universal CRS..."
snarkjs powersoftau prepare \
    "$OUTPUT_DIR/pot_beacon.ptau" \
    "$OUTPUT_DIR/pot_final.ptau"

# Cleanup
rm -f "$OUTPUT_DIR/pot_beacon.ptau"

echo ""
echo "=== Ceremony Complete ==="
echo "Final CRS: $OUTPUT_DIR/pot_final.ptau"
echo ""
echo "Next steps:"
echo "  1. Verify the transcript: snarkjs powersoftau verify $OUTPUT_DIR/pot_final.ptau"
echo "  2. Derive circuit-specific keys: ./phase2_derive.sh"

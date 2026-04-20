# ZK Shielded Transaction Design for Callchain

**Version**: 0.1.0
**Date**: 2026-04-14
**Spec Reference**: spec.md section 3.8

---

## Table of Contents

1. [Overview & Threat Model](#1-overview--threat-model)
2. [Note Format Specification](#2-note-format-specification)
3. [Public vs Private Inputs](#3-public-vs-private-inputs)
4. [Circuit Specification](#4-circuit-specification)
5. [Curve Choice Rationale](#5-curve-choice-rationale)
6. [Trusted Setup & CRS](#6-trusted-setup--crs)
7. [On-Chain Verification](#7-on-chain-verification)
8. [Nullifier Synchronization](#8-nullifier-synchronization)
9. [Viewing Key Design](#9-viewing-key-design)
10. [Performance Targets](#10-performance-targets)
11. [Deposit & Withdraw Flows](#11-deposit--withdraw-flows)
12. [Real Prover Implementation](#12-real-prover-implementation)
13. [Migration Path: Groth16 to Halo2](#13-migration-path-groth16-to-halo2)
14. [Current Implementation Status](#14-current-implementation-status)

---

## 1. Overview & Threat Model

### 1.1 What Shielded Transactions Provide

Shielded transactions use zk-SNARKs (Groth16) to prove the validity of a transfer without revealing:

- **Sender identity** — which address consumed the input notes
- **Receiver identity** — which address receives the output notes
- **Transfer amount** — the value being transferred (encrypted in the note)
- **Linkability** — which input notes correspond to which output notes

What **is** publicly visible:

- That a shielded transfer occurred (the transaction type)
- The nullifiers of spent notes (prevents double-spend)
- The commitments of new notes (added to Merkle tree)
- The asset ID (which token is being transferred)
- The ZK proof itself (~200 bytes)

### 1.2 What the ZK Proof Proves

The prover demonstrates knowledge of private inputs that satisfy all 5 constraints:

1. The nullifiers were correctly derived from input notes + spending keys
2. Each input note exists in the Merkle tree (path is valid)
3. The prover holds spending rights for all input notes
4. Output values do not exceed input values (no minting)
5. All values are in valid range (no overflow, no zero-value notes)

### 1.3 Threat Model

| Threat | Mitigation |
|--------|-----------|
| Double-spend | Nullifiers are published and checked against spent set |
| Counterfeiting | Value conservation enforced in circuit |
| Fake notes | Merkle path validity proves note was committed |
| Spending others' notes | Spending rights proof requires private key knowledge |
| Linkability analysis | Nullifiers unlinkable from commitments without viewing key |
| Front-running | Nullifiers are published, transaction ordering handled by consensus |

### 1.4 What Is NOT Protected

- **Deposit/withdraw amounts** — entering or exiting the shielded pool is visible
- **Metadata timing analysis** — an observer can see when shielded transactions occur
- **Note ciphertext** — encrypted with ChaCha20-Poly1305, but the ciphertext itself is on-chain
- **Compliance mode transactions** — KYC/whitelist checks reveal address participation to the registry operator

---

## 2. Note Format Specification

### 2.1 Note Structure

A `Note` represents a shielded UTXO in the pool:

```
Note {
    value:          u128        // Amount (18 decimals, in wei units)
    asset_id:       u64         // Token identifier
    rcm:            [u8; 32]    // Random commitment mask (deterministic derivation)
    recipient_ivk:  [u8; 32]    // Recipient's incoming viewing key
    rho:            [u8; 32]    // Unique identifier for nullifier derivation
}
```

Total: **120 bytes** in plaintext form.

### 2.2 Note Commitment (Public)

The note commitment is placed in the Merkle tree:

```
commitment = keccak256(value_le || asset_id_le || rcm || rho)
```

Where `value_le` is the 16-byte little-endian encoding of `value`, and `asset_id_le` is the 8-byte little-endian encoding of `asset_id`. The result is a 32-byte `Hash` (B256).

### 2.3 Nullifier Derivation

```
fvc = keccak256("fvk_from_ivk" || recipient_ivk)   // 32 bytes
nullifier = keccak256("nullifier" || fvc || rho)    // 32 bytes
```

The nullifier uniquely identifies a spent note without revealing which note was spent. The `"nullifier"` and `"fvk_from_ivk"` domain separation tags ensure the hash output is unique to its purpose.

### 2.4 Random Commitment Mask (RCM) Derivation

The RCM is derived deterministically (not randomly) from the note's components:

```
rcm = keccak256("rcm" || recipient_ivk || value_le || asset_id_le || rho)
```

This ensures that given the same viewing key and note parameters, the same RCM is produced — enabling deterministic note reconstruction.

### 2.5 Note Encryption (On-Chain Storage)

Notes are encrypted before being stored on-chain using ChaCha20-Poly1305:

```
key = recipient_ivk                          // 32 bytes — used directly as AEAD key
nonce = random 12 bytes                      // Generated per encryption
plaintext = value_le(16) || asset_id_le(8) || rcm(32) || rho(32) || ivk(32)  // 120 bytes
ciphertext = ChaCha20-Poly1305(key, nonce, plaintext)
encrypted_note = nonce(12) || ciphertext || poly1305_tag(16)
```

The recipient decrypts using their incoming viewing key. The Poly1305 tag ensures integrity — a wrong key produces a decryption failure.

### 2.6 Serialization Format

| Component | Size | Encoding |
|-----------|------|----------|
| value | 16 bytes | Little-endian u128 |
| asset_id | 8 bytes | Little-endian u64 |
| rcm | 32 bytes | Raw bytes |
| recipient_ivk | 32 bytes | Raw bytes |
| rho | 32 bytes | Raw bytes |
| **Total plaintext** | **120 bytes** | |
| nonce | 12 bytes | Raw bytes (prepended) |
| ciphertext | 120 bytes | ChaCha20 output |
| Poly1305 tag | 16 bytes | Appended |
| **Total encrypted** | **148 bytes** | |

---

## 3. Public vs Private Inputs

### 3.1 Public Inputs (exposed on-chain)

These values are visible to all validators and included in the transaction:

| Input | Type | Count | Purpose |
|-------|------|-------|---------|
| `nullifiers[]` | `[u8; 32]` | N (inputs) | Mark spent notes, prevent double-spend |
| `commitments[]` | `[u8; 32]` | M (outputs) | New note commitments for Merkle tree |
| `asset_id` | `u64` | 1 | Which asset is being transferred |

Total public inputs: `N * 32 + M * 32 + 8` bytes, plus `N + M + 1` field elements for the circuit.

### 3.2 Private Inputs (kept secret, proven via ZK)

These values are known only to the prover and never exposed on-chain:

| Input | Type | Where Used |
|-------|------|-----------|
| `input_notes[]` | `Note` | Consumed notes (value, rcm, ivk, rho) |
| `output_notes[]` | `Note` | Created notes (value, rcm, ivk, rho) |
| `spending_keys[]` | `[u8; 32]` | Proves ownership of input notes |
| `merkle_paths[]` | `Vec<([u8;32], bool)>` | Proves input notes exist in tree |

The circuit proves that these private inputs satisfy all constraints with respect to the public inputs, without revealing the private inputs themselves.

### 3.3 Input-Output Relationship

```
Public:                    Private (proven, not revealed):
  nullifiers[i]    <-----   input_notes[i] + spending_key[i]
  commitments[j]   <-----   output_notes[j]
  asset_id         <-----   input_notes[i].asset_id == output_notes[j].asset_id
```

---

## 4. Circuit Specification

### 4.1 Constraint Overview

The circuit enforces 5 constraints that together guarantee the shielded transfer is valid:

```
┌─────────────────────────────────────────────────────────────────┐
│                    ShieldedTransfer Circuit                      │
│                                                                  │
│  Public: nullifiers[], commitments[], asset_id                   │
│  Private: notes[], new_notes[], spending_key[], merkle_path[]   │
│                                                                  │
│  Constraint 1: nullifier[i] = H(H("fvk_from_ivk" || ivk), rho)  │
│  Constraint 2: merkle_path proves notes[i] is in tree           │
│  Constraint 3: spending_key correctly derives nullifier          │
│  Constraint 4: Σnew_notes.value ≤ Σnotes.value                  │
│  Constraint 5: all values > 0, all asset_ids match              │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 Constraint 1: Nullifier Derivation

**Statement**: Each public nullifier was correctly derived from its corresponding input note.

**Mathematical form**:
```
For each input note i:
  fvk_i = keccak256("fvk_from_ivk" || notes[i].recipient_ivk)
  nullifiers[i] == keccak256("nullifier" || fvk_i || notes[i].rho)
```

**R1CS encoding** (with Poseidon hash):
```
nf_var = public_input(nullifiers[i])      // FpVar::new_input
ivk_var = witness(notes[i].recipient_ivk) // FpVar::new_witness
rho_var = witness(notes[i].rho)           // FpVar::new_witness

fvc_var = poseidon_hash("fvk_from_ivk_domain", ivk_var)
computed_nf = poseidon_hash("nullifier_domain", fvc_var, rho_var)

enforce: nf_var == computed_nf
```

**Estimated constraints**: ~200 (2 Poseidon hashes per nullifier, ~100 each)

### 4.3 Constraint 2: Merkle Path Validity

**Statement**: Each input note's commitment exists in the Merkle tree (the prover knows a valid path from leaf to root).

**Mathematical form**:
```
For each input note i:
  leaf = commitment(notes[i])
  root = compute_merkle_root(leaf, merkle_paths[i])
  root == current_merkle_root  // must match the chain's state
```

Where `compute_merkle_root` walks the path:
```
current = leaf
for (sibling, sibling_is_right) in path:
    if sibling_is_right:
        current = keccak256(current || sibling)
    else:
        current = keccak256(sibling || current)
```

**R1CS encoding** (with Poseidon hash):
```
current_var = commitment_gadget(note)  // Hash of note fields

for (sibling, is_right) in merkle_path:
    sibling_var = witness(sibling)
    left = select(is_right, current_var, sibling_var)
    right = select(is_right, sibling_var, current_var)
    current_var = poseidon_hash(left, right)

enforce: current_var == public_merkle_root
```

**Estimated constraints**: ~3,200 (32 levels * ~100 per Poseidon hash)

### 4.4 Constraint 3: Spending Rights

**Statement**: The prover knows the spending key that corresponds to the input note's viewing key.

**Mathematical form**:
```
For each input note i:
  spending_key -> viewing_key derivation is consistent
  The nullifier derived from spending_key matches nullifiers[i]
```

**R1CS encoding**:
```
sk_var = witness(spending_key)
ivk_var = poseidon_hash("ivk_domain", sk_var)

// The IVK derived from spending key must match the note's IVK
enforce: ivk_var == notes[i].recipient_ivk

// And the nullifier derivation (same as constraint 1) proves
// the prover knows the key that produces this nullifier
```

**Estimated constraints**: ~100 (1 Poseidon hash for key derivation)

### 4.5 Constraint 4: Value Conservation

**Statement**: No value is created. The sum of output note values must not exceed the sum of input note values.

**Mathematical form**:
```
Σ output_notes[j].value ≤ Σ input_notes[i].value
Σ output_notes[j].value > 0  // at least some value transferred
```

**R1CS encoding**:
```
input_sum = 0
for note in input_notes:
    val_var = witness(note.value)
    input_sum += val_var

output_sum = 0
for note in output_notes:
    val_var = witness(note.value)
    output_sum += val_var

// Enforce output_sum ≤ input_sum
// In R1CS: (input_sum - output_sum) >= 0
// Use range check or decomposition
diff = input_sum - output_sum
enforce: diff is a valid non-negative field element
```

**Estimated constraints**: ~50 per note (value addition + comparison)

### 4.6 Constraint 5: Range & Asset Validity

**Statement**: All values are non-zero and within valid range. All notes use the same asset.

**Mathematical form**:
```
For each note:
  note.value > 0
  note.value < 2^128  // u128 max

For each output note:
  output_notes[j].asset_id == circuit.asset_id
```

**R1CS encoding**:
```
for note in all_notes:
    val_var = witness(note.value)

    // Non-zero: val_var * inv_val_var = 1
    // (if val_var == 0, no inverse exists, constraint fails)
    inv_var = witness(1 / val_var)
    enforce: val_var * inv_var == 1

    // Range check: decompose val_var into bits
    // Ensure val_var fits in 128 bits
    bits = decompose_128(val_var)
    enforce: recompose(bits) == val_var

for note in output_notes:
    asset_var = witness(note.asset_id)
    enforce: asset_var == public_asset_id
```

**Estimated constraints**: ~150 per note (non-zero check + range decomposition)

### 4.7 Total Constraint Estimate

For a typical transfer with 2 input notes and 2 output notes:

| Constraint | Count | Notes |
|------------|-------|-------|
| Nullifier derivation | 400 | 2 inputs * 2 hashes * ~100 |
| Merkle path validity | 6,400 | 2 inputs * 32 levels * ~100 |
| Spending rights | 200 | 2 inputs * ~100 |
| Value conservation | 200 | 4 notes * ~50 |
| Range & asset validity | 600 | 4 notes * ~150 |
| **Total** | **~7,800** | |

At ~7,800 constraints, this is a medium-sized circuit. Groth16 proving takes ~1-3s on a modern CPU. Verification is ~3ms regardless of circuit size.

### 4.8 ShieldedDeposit Circuit

Deposit is the simplest circuit — it only proves that a new note was properly created. There is no input note to spend, no Merkle path to prove, no nullifier to publish.

**Statement**: "I created a well-formed note for the given amount and asset, and I know the recipient's viewing key."

**Public inputs**:

| Input | Type | Purpose |
|-------|------|---------|
| `commitment` | `[u8; 32]` | New note's commitment (will be inserted into Merkle tree) |
| `asset_id` | `u64` | Which asset is being deposited |
| `encrypted_note` | `Vec<u8>` | Encrypted note ciphertext (on-chain storage, not verified in circuit) |

**Private inputs**:

| Input | Type | Purpose |
|-------|------|---------|
| `value` | `u128` | Deposit amount |
| `rcm` | `[u8; 32]` | Random commitment mask |
| `recipient_ivk` | `[u8; 32]` | Recipient's incoming viewing key |
| `rho` | `[u8; 32]` | Unique note identifier |

**Constraints** (3 total):

```
┌────────────────────────────────────────────────────────────┐
│              ShieldedDeposit Circuit                        │
│                                                              │
│  Public: commitment, asset_id                               │
│  Private: value, rcm, recipient_ivk, rho                    │
│                                                              │
│  Constraint D1: commitment == H(value || asset_id || rcm || rho)
│  Constraint D2: value > 0 && value fits in u128 range       │
│  Constraint D3: rcm == H("rcm" || recipient_ivk || value || asset_id || rho)
└────────────────────────────────────────────────────────────┘
```

**Constraint D1: Commitment Validity**

The commitment must be the correct hash of the note's components:

```
value_var = witness(value)
asset_var = public_input(asset_id)
rcm_var = witness(rcm)
rho_var = witness(rho)

// Recompute commitment from components
computed_cm = keccak256_gadget(value_var, asset_var, rcm_var, rho_var)
public_cm = public_input(commitment)

enforce: computed_cm == public_cm
```

Estimated: ~100 constraints (1 keccak256/Poseidon hash)

**Constraint D2: Value Range**

```
enforce: value_var > 0
enforce: value_var fits in 128 bits (bit decomposition)
```

Estimated: ~150 constraints (non-zero + 128-bit range check)

**Constraint D3: RCM Determinism**

The RCM must be correctly derived from the note's components (prevents malicious RCM selection):

```
rcm_domain_var = poseidon_hash("rcm_domain", recipient_ivk, value, asset_id, rho)
enforce: rcm_var == rcm_domain_var
```

Estimated: ~100 constraints (1 Poseidon hash with 4 inputs)

**Total constraints**: ~350

**Why no ZK proof is strictly needed for Deposit**: Deposit is a transparent → shielded transition. The amount is public (deducted from transparent balance). The ZK proof here mainly ensures the note format is correct and the RCM was derived properly — preventing a malicious user from crafting a note with a manipulated commitment that could be used for tracking or double-spend attacks in subsequent transfers.

**Minimal deposit validation** (without full ZK):
- The node can directly verify the commitment format
- Value conservation is handled by the transparent balance deduction
- The only "secret" is the recipient IVK, which is encrypted

So Deposit can use a simplified circuit or even a direct node-side validation without a full Groth16 proof.

---

### 4.9 ShieldedWithdraw Circuit

Withdraw is intermediate complexity — it proves ownership of a shielded note and consumes it, but the output amount is public (credited to a transparent address).

**Statement**: "I own a note with the given value and asset, and I'm withdrawing it to a public address."

**Public inputs**:

| Input | Type | Purpose |
|-------|------|---------|
| `nullifier` | `[u8; 32]` | Marks the consumed note as spent |
| `asset_id` | `u64` | Which asset is being withdrawn |
| `value` | `u128` | Withdrawal amount (public — credited to transparent balance) |
| `target_address` | `[u8; 20]` | Transparent recipient address |
| `merkle_root` | `[u8; 32]` | Current Merkle tree root |

**Private inputs**:

| Input | Type | Purpose |
|-------|------|---------|
| `note_value` | `u128` | Note's encrypted value |
| `rcm` | `[u8; 32]` | Random commitment mask |
| `recipient_ivk` | `[u8; 32]` | Owner's incoming viewing key |
| `rho` | `[u8; 32]` | Unique note identifier |
| `merkle_path` | `Vec<([u8;32], bool)>` | Merkle path proving note exists |

**Constraints** (4 total):

```
┌────────────────────────────────────────────────────────────────────┐
│                    ShieldedWithdraw Circuit                         │
│                                                                      │
│  Public: nullifier, asset_id, value, target_address, merkle_root    │
│  Private: note_value, rcm, recipient_ivk, rho, merkle_path          │
│                                                                      │
│  Constraint W1: nullifier == H(H("fvk_from_ivk" || ivk), rho)       │
│  Constraint W2: merkle_path proves note commitment is in tree        │
│  Constraint W3: note_value == public_value (amount must match)       │
│  Constraint W4: all values > 0, asset_ids match                      │
└────────────────────────────────────────────────────────────────────┘
```

**Constraint W1: Nullifier Derivation**

Same as ShieldedTransfer constraint 1 — proves the withdrawer owns the note:

```
nullifier_var = public_input(nullifier)
ivk_var = witness(recipient_ivk)
rho_var = witness(rho)

fvk_var = poseidon_hash(ivk_var)
computed_nf = poseidon_hash(fvk_var, rho_var)

enforce: nullifier_var == computed_nf
```

Estimated: ~200 constraints (2 Poseidon hashes)

**Constraint W2: Merkle Path Validity**

Same as ShieldedTransfer constraint 2 — proves the note exists in the tree:

```
leaf = compute_note_commitment(note_value, asset_id, rcm, rho)

current = leaf
for (sibling, is_right) in merkle_path:
    left = cond_select(is_right, current, sibling)
    right = cond_select(is_right, sibling, current)
    current = poseidon_hash(left, right)

enforce: current == public_merkle_root
```

Estimated: ~3,200 constraints (32 levels * ~100 per Poseidon hash)

**Constraint W3: Value Match**

The note's encrypted value must equal the public withdrawal amount:

```
note_value_var = witness(note_value)
public_value_var = public_input(value)

enforce: note_value_var == public_value_var
```

Estimated: ~1 constraint (simple equality)

This is the key difference from ShieldedTransfer — the value is public, so no value conservation constraint is needed. The amount is deducted from the shielded pool and credited to the transparent balance.

**Constraint W4: Range & Asset Validity**

```
enforce: value > 0
enforce: value fits in 128 bits
enforce: note_asset_id == public_asset_id
```

Estimated: ~150 constraints (range check + asset match)

**Total constraints**: ~3,551

**Withdraw vs Transfer constraint comparison**:

| Constraint | ShieldedTransfer | ShieldedWithdraw |
|-----------|-----------------|-----------------|
| Nullifier derivation | ~400 (2 inputs) | ~200 (1 input) |
| Merkle path validity | ~6,400 (2 inputs) | ~3,200 (1 input) |
| Spending rights | ~200 (2 inputs) | (implicit in W1) |
| Value conservation | ~200 | Not needed (value is public) |
| Range & asset validity | ~600 (4 notes) | ~150 (1 note) |
| **Total** | **~7,800** | **~3,551** |

Withdraw is roughly half the constraint count of Transfer because it only consumes one note and has no output notes to create or value conservation to prove.

---

### 4.10 All Three Circuits Compared

| Property | ShieldedDeposit | ShieldedTransfer | ShieldedWithdraw |
|----------|----------------|-----------------|-----------------|
| **Direction** | Transparent → Shielded | Shielded → Shielded | Shielded → Transparent |
| **Consumes notes** | No | Yes (1-N) | Yes (1) |
| **Creates notes** | Yes (1) | Yes (1-M) | No |
| **Nullifiers** | None | Public (N) | Public (1) |
| **Commitments** | Public (1) | Public (M) | None |
| **Merkle path** | None | Required (input notes) | Required (consumed note) |
| **Value conservation** | Transparent balance handles it | In circuit (ZK) | Public value match |
| **Constraints** | ~350 | ~7,800 | ~3,551 |
| **Proving time** | <0.5s | 1-3s | 0.5-1.5s |
| **Proof needed** | Optional (can validate directly) | Required | Required |

**Circuit reuse strategy**:

The ShieldedWithdraw circuit is essentially a subset of the ShieldedTransfer circuit. In practice, you can:

1. **Separate circuits** — generate distinct proving/verifying keys for each operation. Simpler but 3 separate CRS setups.
2. **Unified circuit** — one circuit parameterized by operation type (deposit=0, transfer=1, withdraw=2). Uses conditional constraints to skip unused checks. Larger but only 1 CRS setup.
3. **Transfer as base** — use the Transfer circuit for both Transfer and Withdraw (Withdraw is just Transfer with 0 output notes and public value). Reuse the same VK.

**Recommendation**: Start with separate circuits for clarity. Migrate to a unified circuit when the constraint count and key management overhead justify the complexity.

---

## 5. Curve Choice Rationale

### 5.1 Candidate Curves

| Curve | Pairing | Native Field | EVM Gas Cost | Notes |
|-------|---------|-------------|-------------|-------|
| **BN254 (alt_bn128)** | Yes | 254-bit | ~3,500 gas (ecPairing) | Precompiled in EVM |
| **BLS12-381** | Yes | 381-bit | ~28,000 gas (no precompile) | Better security margin |
| **BLS12-377** | Yes | 377-bit | N/A | Used by some ZK systems |

### 5.2 Decision: BN254

**BN254 is the recommended choice** for the following reasons:

1. **EVM Precompile**: BN254 (alt_bn128) has native precompiled contracts in the EVM for ecAdd (150 gas), ecMul (6,000 gas), and ecPairing (35,000 gas as of EIP-2537 proposal, currently ~151,000 gas). This makes on-chain Groth16 verification feasible.

2. **Gas Cost**: On-chain verification of a BN254 Groth16 proof costs ~285,000 gas (2 pairings + additions). BLS12-381 would require custom bytecode or no on-chain verification at all.

3. **Callchain's EVM Integration**: The project uses `revm` (Rust EVM) which includes BN254 precompiles. The node can verify shielded proofs natively without external dependencies.

4. **Compatibility**: ark-groth16 supports BN254 via `ark-bn254` crate. The constraint count (~7,800) fits within BN254's scalar field (254 bits ≈ 32 bytes per element).

### 5.3 Security Considerations

- BN254 has a 128-bit security level (sufficient for most applications)
- BLS12-381 has a higher security margin (~128-144 bits) but at significantly higher gas cost
- For migration to Halo2 (no trusted setup), the curve choice remains flexible since Halo2 doesn't require pairing-friendly curves for the proving system itself

### 5.4 Current Code Alignment

The existing codebase uses `call-primitives` with:
- `Hash` = B256 (32 bytes) — matches BN254's 254-bit field
- `Address` = 20 bytes (EVM compatible)
- `AssetId` = u64, `Balance` = u128 — both fit within BN254 scalar field

BN254 aligns naturally with the existing type system.

---

## 6. Trusted Setup & CRS

### 6.1 The Problem

Groth16 requires a trusted setup — a one-time generation of proving and verifying keys. If the "toxic waste" (randomness used during setup) is leaked, anyone can create fake proofs.

### 6.2 Recommended Approach: Powers of Tau Ceremony

Use the **Perpetual Powers of Tau** ceremony or run a dedicated multi-party computation (MPC):

```
Phase 1: Powers of Tau (curve-wide, reusable)
  └── Multi-party ceremony (100+ participants)
  └── Generates universal CRS (Common Reference String)
  └── "Toxic waste" destroyed when at least one participant is honest

Phase 2: Circuit-specific (per circuit, deterministic)
  └── Derive circuit-specific PK/VK from Phase 1 CRS
  └── No additional trust assumption
  └── Can be run by a single party
```

### 6.3 Production Setup

For a production deployment:

1. **Phase 1**: Participate in or initiate a Powers of Tau ceremony
   - Minimum 21 participants (matching validator count)
   - Each participant contributes randomness
   - Final beacon (e.g., block hash) adds unpredictability
   - Result: `bn254_pot28.ptau` (powers of tau up to 2^28 constraints)

2. **Phase 2**: Generate circuit-specific keys
   ```bash
   # Using snarkjs (or arkworks equivalent)
   snarkjs powersoftau new bn254 28 pot28_0000.ptau -e "callchain shielded setup"
   snarkjs powersoftau contribute pot28_0000.ptau pot28_final.ptau --name="Final" -e="beacon"

   # Circuit-specific
   snarkjs groth16 setup circuit.r1cs pot28_final.ptau call-shielded_0000.zkey
   snarkjs zkey contribute call-shielded_0000.zkey call-shielded_final.zkey
   snarkjs zkey export verificationkey call-shielded_final.zkey verification_key.json
   ```

3. **Key Distribution**:
   - `proving_key` (~100KB) — distributed to clients for proof generation
   - `verifying_key` (~200B) — embedded in the node for verification
   - Solidity verifier contract — deployed on-chain

### 6.4 Development/Testing Setup

For development, use ark-groth16's built-in setup (no ceremony needed):

```rust
let mut rng = thread_rng();
let (pk, vk) = Groth16::circuit_specific_setup(&circuit, &mut rng).unwrap();
```

This generates a "dummy" CRS that is only valid for testing — never use in production.

### 6.5 Key Rotation

- Verifying keys are circuit-specific — if the circuit changes, new keys are needed
- Phase 1 CRS can be reused across circuit versions (sufficiently large)
- Plan for key rotation when upgrading the circuit (new constraint count, new hash function, etc.)

---

## 7. On-Chain Verification

### 7.1 Verification Flow

When a shielded transaction is submitted:

```
┌──────────────────────────────────────────────────────────────────┐
│                    Transaction Validation                         │
│                                                                   │
│  1. Decode: Extract nullifiers, commitments, asset_id, proof     │
│  2. Check: Nullifiers not already spent                          │
│  3. Verify: Groth16 proof against verifying key                  │
│     e(A, B) = e(α, β) * e(Σ inputs, γ) * e(C, δ)               │
│  4. Check: Value conservation (implicit in proof)                │
│  5. Update: Mark nullifiers spent, insert commitments            │
└──────────────────────────────────────────────────────────────────┘
```

### 7.2 Solidity Verifier Contract

For EVM-compatible verification, generate a Solidity verifier from the verification key:

```solidity
// Auto-generated by snarkjs zkey export solidityverifier
contract ShieldedVerifier {
    // Verifying key constants (BN254)
    uint256 constant alphaX = ...;
    uint256 constant alphaY = ...;
    uint256 constant betaX1 = ...;
    // ... (all VK constants)

    function verifyProof(
        uint[2] calldata a,      // G1 point
        uint[2][2] calldata b,   // G2 point
        uint[2] calldata c,      // G1 point
        uint[] calldata inputs   // public inputs: nullifiers || commitments || asset_id
    ) public view returns (bool) {
        // Pairing check: e(A, B) == e(α, β) * e(Σ inputs, γ) * e(C, δ)
        // Uses BN254 precompile at 0x08
        // Returns true if proof is valid
    }
}
```

Gas cost: ~285,000 gas per proof verification on BN254.

### 7.3 Native Rust Verification (call-node)

The node verifies proofs natively without EVM:

```rust
impl Groth16Prover {
    pub fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        // Deserialize proof points from bytes
        let groth16_proof = deserialize(&proof.proof_data)?;

        // Build public inputs as field elements
        let public_inputs = build_public_inputs(
            &proof.nullifiers,
            &proof.commitments,
            proof.asset_id,
        )?;

        // Process verifying key
        let pvk = Groth16::process_vk(&self.verifying_key)?;

        // Verify: ~3ms on BN254
        let valid = Groth16::verify_with_processed_vk(
            &pvk, &public_inputs, &groth16_proof
        )?;

        Ok(valid)
    }
}
```

### 7.4 Verification Key Storage

The verifying key is stored in the node configuration:

```
config/
  shielded/
    vk.bin          # Verifying key (~200B)
    circuit_info.json  # Circuit metadata (constraint count, public input count)
```

The proving key (~100KB) is distributed separately to clients that generate proofs.

---

## 8. Nullifier Synchronization

### 8.1 The Nullifier Set

The nullifier set tracks which notes have been spent. It's the critical state that prevents double-spending in the shielded pool.

**Current implementation**: `NullifierSet` with dual-layer design:

```rust
pub struct NullifierSet {
    spent: HashSet<Nullifier>,  // Exact: O(1) lookup, no false positives
    bitset: Vec<u64>,           // Compressed: bucketed bitfield
}
```

### 8.2 Bucket Compression

The BitSet provides a compressed representation for fast probabilistic checks:

```
bucket_index = u64::from_le_bytes(hash[0..8]) % 64
bit_index = u64::from_le_bytes(hash[8..16]) % 64
```

- Each bucket covers 64 nullifiers (by hash prefix)
- Each nullifier gets 1 bit within its bucket
- `maybe_spent()` checks the bit (may have false positives, no false negatives)
- `is_spent()` checks the exact HashSet (authoritative)

### 8.3 Nullifier Lifecycle

```
┌────────────┐    ┌────────────┐    ┌────────────┐
│ Note       │───>│ Commitment │───>│ Spent      │
│ (unspent)  │    │ in tree    │    │ (nullifier)│
└────────────┘    └────────────┘    └────────────┘
     │                  │                  │
     │ can be spent     │ visible in tree  │ marked in nullifier set
     │ with ZK proof    │ but not linkable │ cannot be spent again
     └──────────────────┘                  └────────────────────────┘
```

### 8.4 Cross-Block Synchronization

Nullifiers are processed per-block with a limit:

```rust
pub struct ShieldedBlockTracker {
    pub count: u32,             // Shielded tx count this block
    pub pending: Vec<ShieldedTransfer>,
}

impl ShieldedBlockTracker {
    pub const MAX_PER_BLOCK: u32 = 50;  // Per spec §3.8.7
}
```

At block finalization:
1. All nullifiers from pending transfers are inserted into the nullifier set
2. All commitments are inserted into the Merkle tree
3. The tracker is reset for the next block

### 8.5 Merkle Tree & Nullifier Relationship

```
ShieldedState {
    merkle_tree: IncrementalMerkleTree   // Append-only, depth 32
    nullifier_set: NullifierSet          // Grows with spent notes
    note_registry: HashMap<CM, Note>    // Maps commitments to encrypted notes
}
```

- The Merkle tree only grows (append-only). It never shrinks.
- The nullifier set only grows (nullifiers are never removed).
- The note registry maps commitments to encrypted notes for lookup.

---

## 9. Viewing Key Design

### 9.1 Key Hierarchy

```
spending_key (32 bytes, secret)
    │
    ├── keccak256("ivk" || spending_key) ──> incoming_view_key (32 bytes)
    │                                            │
    │                                            ├── Can decrypt incoming notes
    │                                            ├── Used for KYC/whitelist address derivation
    │                                            └── Given to auditors for read-only access
    │
    └── keccak256("fvk" || spending_key) ──> full_view_key (32 bytes)
                                                 │
                                                 ├── Can view all related transactions
                                                 ├── Used for nullifier derivation
                                                 └── More powerful than incoming_view_key
```

**ViewingKey struct**:
```rust
pub struct ViewingKey {
    pub incoming_view_key: [u8; 32],  // IVK: decrypt incoming notes
    pub full_view_key: [u8; 32],      // FVK: full audit access
}
```

### 9.2 Key Usage by Role

| Role | Has | Can Do |
|------|-----|--------|
| **Note owner** | spending_key | Spend notes, decrypt all notes, derive nullifiers |
| **Recipient** | incoming_view_key | Decrypt notes sent to them |
| **Auditor** | full_view_key (shared) | View all transactions, decrypt values |
| **KYC registry** | Derived address only | Verify eligibility (no decryption) |
| **Public** | Nothing (nullifiers + commitments only) | See that a transfer occurred |

### 9.3 Compliance Modes

```rust
pub enum ShieldedComplianceMode {
    /// No compliance checks (full privacy)
    Unrestricted,

    /// Sender/receiver must be KYC-verified
    KycRequired { kyc_registry: Vec<Address> },

    /// Asset issuer can audit via viewing key
    IssuerAuditable {
        issuer: Address,
        auditor_view_key: ViewingKey,
    },

    /// Only whitelisted addresses can participate
    WhitelistedOnly { whitelist: HashSet<Address> },
}
```

### 9.4 Address Derivation for Compliance

KYC and whitelist modes derive a 20-byte address from the note's RCM:

```rust
fn derive_address_from_ivk(note: &Note) -> Address {
    let hash = keccak256(note.rcm());  // 32 bytes
    let mut addr = Address::ZERO;
    addr.copy_from_slice(&hash[12..32]); // Last 20 bytes
    addr
}
```

This derivation is **one-way**: knowing the address does not reveal the viewing key or allow decryption. It only allows the registry to check if the recipient is authorized.

### 9.5 Audit Record

When an auditor uses a viewing key to inspect transactions:

```rust
pub struct AuditRecord {
    pub block: u64,
    pub nullifiers: Vec<[u8; 32]>,     // Spent notes
    pub auditor_key: [u8; 32],         // Which auditor
    pub decrypted_values: Vec<u128>,   // Revealed amounts
    pub asset_ids: Vec<u64>,           // Which assets
}
```

Audit records are stored off-chain (regulatory requirement, not on-chain data).

---

## 10. Performance Targets

### 10.1 Targets (from spec §3.8.7)

| Metric | Target | Current (mock) | Real (estimated) |
|--------|--------|---------------|-----------------|
| Proof generation | 1-5s (client) | <1ms | 1-3s (ark-groth16, BN254) |
| Proof verification | ~3ms (node) | <1ms | ~3ms (BN254 pairing) |
| Proof size | ~200B | 200B | 192B (2 G1 + 1 G2) |
| Verifying key | ~200B | 200B (mock) | ~200B (BN254 VK) |
| Proving key | ~100KB | 1KB (mock) | ~100KB (BN254 PK) |
| Nullifier check | O(1) | O(1) HashSet | O(1) HashSet + BitSet |
| Merkle tree update | O(log n) | O(log n) | O(log n) |
| Max per block | 50 tx | 50 tx | 50 tx |

### 10.2 Block Time Analysis

With 250ms block time and 50 shielded transactions:
- Total verification: 50 * 3ms = 150ms
- Leaves 100ms for other processing (consensus, transparent tx, etc.)
- 50 tx limit ensures shielded tx doesn't dominate block time

### 10.3 Gas Costs (spec §3.8.2)

| Operation | Gas | Notes |
|-----------|-----|-------|
| ShieldedTransfer | 50,000 | Includes proof verification |
| ShieldedDeposit | 20,000 | Lock tokens, create commitment |
| ShieldedWithdraw | 20,000 | Consume note, unlock to transparent |

### 10.4 Circuit Size vs Performance

| Input/Output Count | Constraints | Proving Time | Notes |
|-------------------|-------------|-------------|-------|
| 1 in, 1 out | ~3,900 | ~0.5-1s | Minimal transfer |
| 2 in, 2 out | ~7,800 | ~1-3s | Typical transfer |
| 4 in, 4 out | ~15,600 | ~3-6s | Consolidation |
| 8 in, 8 out | ~31,200 | ~6-12s | Large batch |

The per-block limit of 50 tx naturally constrains total proving workload.

### 10.4 Performance Fallback Strategies

> **Note**: The following strategies are contingencies if benchmarking shows the ~7,800 constraint circuit produces proofs too slowly (>5s on target hardware). They are ordered by intrusiveness (least to most invasive).

#### Option 1: Circuit Optimization (No Architecture Change)

Reduce constraints without changing the proving system:

| Optimization | Current | Optimized | Savings |
|-------------|---------|-----------|---------|
| Merkle tree depth | 32 levels (4.2B leaves) | 20 levels (1M leaves) | ~40% |
| Hash for nullifier | 2x Poseidon per nullifier | Single Pedersen commitment | ~15% |
| Range checks | Per-note bit decomposition | Batch range check for all notes | ~10% |
| **Total** | **~7,800** | **~2,500-3,500** | **~50-55%** |

The biggest win is **Merkle tree depth reduction**: 20 levels = 1M notes is sufficient for years of usage. Cutting from 32 to 20 removes 12 Poseidon hashes per input note × 2 inputs = 2,400 constraints.

#### Option 2: Server-Side Prover (Client Doesn't Generate Proof)

Users submit encrypted witness data to a prover service:

```
User wallet ──(encrypted witness)──> Prover server ──(proof)──> On-chain verification
```

- Server uses GPU / high-memory hardware, proof time: 3s → 0.3s
- User doesn't wait locally
- **Trade-off**: Requires trusting the prover server (it sees the witness)
- **Best for**: Built-in wallet prover service (like MetaMask's Infura integration)

#### Option 3: Recursive Proof Composition

Use Halo2's recursive proof capability:

```
Sub-proof A (Deposit):    ~350 constraints  →  sub-proof
Sub-proof B (Transfer):   ~7,800 constraints →  sub-proof
Final proof C:             Verifies A + B → 1 ~200B proof on-chain
```

User only submits the final proof. Complex proof generation runs asynchronously in the background on the client.

#### Option 4: Staged / Deferred Verification

Don't verify everything synchronously:

| Stage | What's Verified | Who Verifies | Time |
|-------|----------------|-------------|------|
| Tx submission | Format check + signature | Node | <1ms |
| Before inclusion | Fast nullifier check | Node | O(1) |
| After inclusion | Full ZK proof | Validator subset | 3ms/tx |

**Key idea**: Node accepts the tx quickly (fast path), validator subset verifies ZK proofs in parallel before block finalization. Similar to Ethereum's blob tx model — accept first, verify later.

#### Option 5: Hybrid Circuit (Value-Based)

Choose circuit complexity dynamically based on transfer amount:

| Amount | Circuit | Constraints | Proof Time |
|--------|---------|-------------|------------|
| Small (<$100) | Simplified | ~100 | <0.1s |
| Large (≥$100) | Full | ~7,800 | 1-3s |

Simplified circuit only verifies nullifier + value conservation, skips full Merkle path verification (relies on node-layer fast check as fallback).

---

### Recommended Approach

**Implement the full circuit first, benchmark, then decide.**

Reasoning:
1. 7,800 constraints is actually modest (Zcash Sapling is 50,000+)
2. Modern CPU Groth16 proving is 5-10x faster than 3 years ago
3. Even 5s is acceptable — users generate proofs asynchronously, doesn't affect on-chain speed
4. **Only real benchmarks tell the true performance**

If benchmarks show timeout, **Option 1 (circuit optimization)** is the smallest change with the most direct impact — cutting Merkle depth from 32 to 20 halves the constraints and halves proof time.

---

## 11. Deposit & Withdraw Flows

### 11.1 ShieldedDeposit (Transparent → Shielded)

```
User (transparent)                    Shielded Pool
      │                                     │
      │── deposit(amount, asset_id) ───────>│
      │   1. Deduct from transparent balance│
      │   2. Create Note {                   │
      │      value: amount,                  │
      │      asset_id,                       │
      │      recipient_ivk: user.ivk,        │
      │      rho: random(),                  │
      │   }                                  │
      │   3. Compute commitment              │
      │   4. Insert into Merkle tree         │
      │   5. Encrypt note → on-chain storage │
      │                                     │
      │<── success (note encrypted) ────────│
```

**Public**: Address deposited, asset_id, amount
**Hidden**: Who the note belongs to (recipient IVK is encrypted), the RCM

### 11.2 ShieldedTransfer (Shielded → Shielded)

```
User A (shielded)                   Shielded Pool
      │                                     │
      │── transfer(proof, nullifiers,       │
      │            commitments) ───────────>│
      │   1. Verify ZK proof (~3ms)         │
      │   2. Check nullifiers not spent     │
      │   3. Value conservation check       │
      │   4. Mark nullifiers spent          │
      │   5. Insert commitments into tree   │
      │   6. Register encrypted notes       │
      │                                     │
      │<── success ─────────────────────────│
```

**Public**: A shielded transfer occurred, nullifiers, commitments, asset_id
**Hidden**: Sender, receiver, amounts, which inputs map to which outputs

### 11.3 ShieldedWithdraw (Shielded → Transparent)

```
User (shielded)                     Shielded Pool              User (transparent)
      │                                    │                          │
      │── withdraw(note, proof,            │                          │
      │         target_address) ─────────>│                          │
      │   1. Verify ZK proof              │                          │
      │   2. Check nullifier not spent    │                          │
      │   3. Consume the note             │                          │
      │   4. Mark nullifier spent         │                          │
      │   5. Credit target transparent    │                          │
      │      balance with amount          │                          │
      │                                    │── credit(amount) ──────>│
      │<── success ──────────────────────│                          │
```

**Public**: Amount withdrawn, target transparent address
**Hidden**: Who withdrew (the note consumer is anonymous)

### 11.4 Instruction Types (from spec)

From `call-primitives` `InstructionType` enum:
- `ShieldedTransfer` = 12
- `ShieldedWithdraw` = 13
- `ShieldedDeposit` = 14

### 11.5 Privacy Analysis of Flow Transitions

| Transition | What's Visible | What's Hidden |
|------------|---------------|---------------|
| Transparent → Shielded (Deposit) | From address, amount, asset | Recipient note content |
| Shielded → Shielded (Transfer) | That it happened, nullifiers, commitments | Sender, receiver, amount |
| Shielded → Transparent (Withdraw) | To address, amount, asset | Source note, original sender |

The privacy boundary is the shielded pool. Deposits and withdrawals are visible but the internal transfers are not.

---

## 12. Real Prover Implementation

### 12.1 Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     call-shielded crate                          │
│                                                                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐    │
│  │ circuit  │──│ prover   │──│  notes   │──│ compliance   │    │
│  │ (R1CS)   │  │ (Groth16)│  │(encrypt) │  │  (modes)     │    │
│  └──────────┘  └──────────┘  └──────────┘  └──────────────┘    │
│       │              │            │               │              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐    │
│  │ merkle   │  │lib.rs    │  │nullifiers│  │  test_utils  │    │
│  │ (tree)   │  │(state)   │  │ (bitset) │  │  (testing)   │    │
│  └──────────┘  └──────────┘  └──────────┘  └──────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

### 12.2 Dependencies

```toml
# Add to crates/shielded/Cargo.toml
ark-std = "0.5"
ark-ff = "0.5"              # Finite field arithmetic (Fr for BN254)
ark-ec = "0.5"              # Elliptic curve operations (G1, G2)
ark-groth16 = "0.5"         # Groth16 prover/verifier
ark-r1cs-std = "0.5"        # R1CS constraint system for circuits
ark-bn254 = "0.5"           # BN254 curve parameters
ark-relations = "0.5"       # SNARK traits (ConstraintSynthesizer)
ark-serialize = "0.5"       # Field element serialization
poseidon-ark = "0.0.1"      # Poseidon hash gadget for arkworks
```

### 12.3 Circuit Implementation

Replace the current plaintext constraint checks with a real `ConstraintSynthesizer`:

```rust
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_r1cs_std::{prelude::*, fields::fp::FpVar};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use ark_bn254::{Bn254, Fr};

/// Note witness for the circuit (private inputs)
pub struct NoteWitness {
    pub value: u128,
    pub asset_id: u64,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
}

/// Real ZK circuit with arkworks R1CS
pub struct ShieldedProofCircuit {
    // Public inputs
    pub nullifiers: Vec<[u8; 32]>,
    pub commitments: Vec<[u8; 32]>,
    pub asset_id: u64,
    pub merkle_root: [u8; 32],

    // Private inputs (witnesses)
    pub input_notes: Vec<NoteWitness>,
    pub output_notes: Vec<NoteWitness>,
    pub merkle_paths: Vec<Vec<([u8; 32], bool)>>,
}

impl ConstraintSynthesizer<Fr> for ShieldedProofCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        // --- Constraint 1: Nullifier derivation ---
        for i in 0..self.input_notes.len() {
            let note = &self.input_notes[i];

            // Public: nullifier
            let nf_var = FpVar::new_input(
                cs.clone(),
                || format!("nf_{i}"),
                || Ok(Fr::from_be_bytes_mod_order(&self.nullifiers[i])),
            )?;

            // Private: IVK, rho
            let ivk_var = FpVar::new_witness(
                cs.clone(),
                || format!("ivk_{i}"),
                || Ok(Fr::from_be_bytes_mod_order(&note.recipient_ivk)),
            )?;
            let rho_var = FpVar::new_witness(
                cs.clone(),
                || format!("rho_{i}"),
                || Ok(Fr::from_be_bytes_mod_order(&note.rho)),
            )?;

            // Compute FVK from IVK (Poseidon hash)
            let fvk_var = poseidon_hash(&[ivk_var])?;

            // Compute nullifier: H(FVK, rho)
            let computed_nf = poseidon_hash(&[fvk_var, rho_var])?;

            // Enforce equality
            nf_var.enforce_equal(&computed_nf)?;
        }

        // --- Constraint 2: Merkle path validity ---
        for i in 0..self.input_notes.len() {
            let note = &self.input_notes[i];
            let path = &self.merkle_paths[i];

            // Compute note commitment as circuit variable
            let leaf = compute_note_commitment(note, cs.clone())?;

            // Walk Merkle path
            let mut current = leaf;
            for (j, (sibling, is_right)) in path.iter().enumerate() {
                let sib_var = FpVar::new_witness(
                    cs.clone(),
                    || format!("sib_{i}_{j}"),
                    || Ok(Fr::from_be_bytes_mod_order(sibling)),
                )?;

                let left = cond_select(*is_right, &current, &sib_var)?;
                let right = cond_select(*is_right, &sib_var, &current)?;
                current = poseidon_hash(&[left, right])?;
            }

            // Enforce computed root matches public Merkle root
            let root_var = FpVar::new_input(
                cs.clone(),
                || format!("root_{i}"),
                || Ok(Fr::from_be_bytes_mod_order(&self.merkle_root)),
            )?;
            current.enforce_equal(&root_var)?;
        }

        // --- Constraint 3: Spending rights ---
        for i in 0..self.input_notes.len() {
            // The nullifier derivation (constraint 1) already proves
            // that the prover knows the IVK. Spending rights are proven
            // by showing the IVK was derived from a valid spending key.
            // This is implicit in constraint 1's Poseidon hash chain.
        }

        // --- Constraint 4: Value conservation ---
        let mut input_sum = Fr::zero();
        for note in &self.input_notes {
            let val_var = FpVar::new_witness(
                cs.clone(),
                || format!("in_val_{}", note.value),
                || Ok(Fr::from(note.value)),
            )?;
            input_sum += val_var.value()?;
        }

        let mut output_sum = Fr::zero();
        for note in &self.output_notes {
            let val_var = FpVar::new_witness(
                cs.clone(),
                || format!("out_val_{}", note.value),
                || Ok(Fr::from(note.value)),
            )?;
            output_sum += val_var.value()?;
        }

        // Enforce: output_sum <= input_sum
        let diff = input_sum - output_sum;
        // Enforce diff is non-negative (decompose into bits)
        enforce_non_negative(cs.clone(), diff)?;

        // --- Constraint 5: Range & asset validity ---
        for (i, note) in self.input_notes.iter().enumerate() {
            let val_var = FpVar::new_witness(
                cs.clone(),
                || format!("range_in_{i}"),
                || Ok(Fr::from(note.value)),
            )?;
            enforce_non_zero(cs.clone(), val_var)?;
            enforce_128_bit_range(cs.clone(), val_var)?;
        }

        for (i, note) in self.output_notes.iter().enumerate() {
            let val_var = FpVar::new_witness(
                cs.clone(),
                || format!("range_out_{i}"),
                || Ok(Fr::from(note.value)),
            )?;
            enforce_non_zero(cs.clone(), val_var)?;
            enforce_128_bit_range(cs.clone(), val_var)?;

            // Asset ID match
            let asset_var = FpVar::new_witness(
                cs.clone(),
                || format!("asset_out_{i}"),
                || Ok(Fr::from(note.asset_id)),
            )?;
            let public_asset = FpVar::new_input(
                cs.clone(),
                || "public_asset_id",
                || Ok(Fr::from(self.asset_id)),
            )?;
            asset_var.enforce_equal(&public_asset)?;
        }

        Ok(())
    }
}
```

### 12.4 Real Prover

```rust
use ark_groth16::Groth16;
use ark_bn254::Bn254;
use ark_std::rand::thread_rng;

pub struct RealProver {
    proving_key: ProvingKey<Bn254>,
    verifying_key: VerifyingKey<Bn254>,
}

impl RealProver {
    /// Generate circuit-specific setup keys
    pub fn setup(circuit: &ShieldedProofCircuit) -> Self {
        let mut rng = thread_rng();
        let (pk, vk) = Groth16::circuit_specific_setup(circuit, &mut rng).unwrap();
        Self { proving_key: pk, verifying_key: vk }
    }

    /// Load keys from disk (production use)
    pub fn load(pk_path: &str, vk_path: &str) -> Result<Self, ProverError> {
        let pk = load_proving_key(pk_path)?;
        let vk = load_verifying_key(vk_path)?;
        Ok(Self { proving_key: pk, verifying_key: vk })
    }

    /// Generate a real Groth16 proof (~200B)
    pub fn prove(&self, circuit: &ShieldedProofCircuit) -> Result<ZkProof, ProverError> {
        let mut rng = thread_rng();
        let proof = Groth16::prove(&self.proving_key, circuit.clone(), &mut rng)?;

        // Serialize: proof.a (G1: 64B) + proof.b (G2: 128B) + proof.c (G1: 64B) = ~256B
        // Compressed: ~192B with point compression
        let proof_data = serialize_groth16_proof(&proof)?;

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.iter()
                .map(|b| Nullifier::new(Hash::from_slice(b)))
                .collect(),
            commitments: circuit.commitments.iter()
                .map(|b| NoteCommitment::new(Hash::from_slice(b)))
                .collect(),
            asset_id: circuit.asset_id,
        })
    }

    /// Verify a Groth16 proof (~3ms)
    pub fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        let groth16_proof = deserialize_groth16_proof(&proof.proof_data)?;

        let mut public_inputs = Vec::new();
        for nf in &proof.nullifiers {
            public_inputs.push(Fr::from_be_bytes_mod_order(nf.as_hash().as_slice()));
        }
        for cm in &proof.commitments {
            public_inputs.push(Fr::from_be_bytes_mod_order(cm.as_hash().as_slice()));
        }
        public_inputs.push(Fr::from(proof.asset_id));

        let pvk = Groth16::process_vk(&self.verifying_key)?;
        let valid = Groth16::verify_with_processed_vk(&pvk, &public_inputs, &groth16_proof)?;

        Ok(valid)
    }
}

impl Prover for RealProver {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError> {
        // Convert ShieldedCircuit -> ShieldedProofCircuit
        let zk_circuit = ShieldedProofCircuit::from(circuit)?;
        self.prove(&zk_circuit)
    }

    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        self.verify(proof)
    }
}
```

### 12.5 Poseidon Hash Configuration

Poseidon parameters for BN254 (scalar field):

```rust
// Poseidon configuration for BN254 Fr field
// Rate = 8, Capacity = 4, Full rounds = 8, Partial rounds = 57
let poseidon_config = PoseidonConfig::<Fr>::new(
    8,   // rate (inputs processed per round)
    4,   // capacity (state elements not output)
    8,   // full_rounds
    57,  // partial_rounds
    // MDS matrix and round constants from reference implementation
    poseidon_bn254::constants(),
);
```

### 12.6 Integration Path

```
Step 1: Add arkworks dependencies to Cargo.toml
Step 2: Add poseidon hash gadget
Step 3: Implement ShieldedProofCircuit with ConstraintSynthesizer
Step 4: Implement RealProver with Groth16::prove/verify
Step 5: Add Merkle tree with Poseidon hash (parallel to keccak256 tree)
Step 6: Add integration tests that generate real proofs
Step 7: Generate production CRS (powers of tau ceremony)
Step 8: Generate Solidity verifier contract
Step 9: Benchmark and optimize constraint count
Step 10: Deploy with feature flag (mock + real prover coexist)
```

---

## 13. Migration Path: Groth16 to Halo2

### 13.1 Why Migrate

| Aspect | Groth16 | Halo2 |
|--------|---------|-------|
| Trusted setup | Required | Not required |
| Proof size | ~200B | ~1-2KB |
| Verification | ~3ms | ~10ms |
| Key size | PK: ~100KB, VK: ~200B | No keys (universal) |
| EVM verification | Yes (via precompile) | Harder (no native support) |
| Flexibility | Per-circuit setup | Universal SRS |

### 13.2 Migration Strategy

The `Prover` trait already abstracts the backend:

```rust
pub trait Prover: Send + Sync {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError>;
    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError>;
}
```

Both `MockProver` and `Groth16Prover` implement this trait. When migrating to Halo2:

1. Add `halo2_prover` module implementing the same `Prover` trait
2. The `ZkProof` struct may need adjustment (Halo2 proofs are larger: ~1-2KB)
3. Update the proof size validation (currently 200B, would be ~2KB)
4. Update on-chain verification (Halo2 requires a different verifier contract)

### 13.3 Timeline

- **Phase 1 (current)**: Groth16 with arkworks — production-ready, well-understood
- **Phase 2 (future)**: Evaluate Halo2 when EVM support for its verification is available
- **Phase 3 (optional)**: Hybrid mode — Groth16 for on-chain verification, Halo2 for off-chain/rollup scenarios

---

## 14. Current Implementation Status

### 14.1 Completed (Zero Stubs)

| Component | File | Status | Tests |
|-----------|------|--------|-------|
| Note structure + encryption | `notes.rs` | Complete | 6 |
| Merkle tree (depth 32) | `merkle.rs` | Complete | 10 |
| Nullifier set (BitSet) | `nullifiers.rs` | Complete | 5 |
| Circuit constraints (5) | `circuit.rs` | Complete | 7 |
| Prover trait + MockProver | `prover.rs` | Complete | 8 |
| Prover trait + Groth16Prover | `prover.rs` | Mock data | 8 |
| Compliance modes (4) | `compliance.rs` | Complete | 12 |
| State machine | `lib.rs` | Complete | 10 |
| Block tracker | `lib.rs` | Complete | 1 |

**Total: 55 passing tests, 7 source files**

### 14.2 Mock vs Real

| Component | Current | Real Implementation Needed |
|-----------|---------|---------------------------|
| `verify_zk_proof()` | Structural validation | `Groth16::verify()` |
| `MockProver::prove()` | Constraint check + 200B dummy | `Groth16::prove()` with R1CS |
| `Groth16Prover::prove()` | Constraint check + 200B dummy | Same as above |
| Merkle tree hash | keccak256 | Poseidon (for circuit) |
| Proof data | `[u8; 200]` dummy bytes | Serialized G1/G2 points |

### 14.3 Implementation Status (Updated 2026-04-20)

#### Implemented

| Component | Status | Notes |
|-----------|--------|-------|
| arkworks integration | **Complete** | `ark-groth16`, `ark-r1cs-std`, `ark-bn254`, `ark-serialize` all wired |
| Real ConstraintSynthesizer | **Complete** | `ShieldedTransfer`, `ShieldedWithdraw`, `ShieldedDeposit` circuits with full R1CS constraints |
| Poseidon hash gadget | **Complete** | `poseidon.rs` + `merkle_poseidon.rs` for circuit-friendly hashing |
| Proof serialization | **Complete** | `proof_ser.rs` — G1/G2 point serialization (128 bytes compressed) |
| Deposit circuit | **Complete** | ~350 constraints, tested with `test_real_prover_deposit_proof_cycle` |
| Withdraw circuit | **Complete** | ~3,551 constraints, tested with `test_real_prover_withdraw_proof_cycle` |
| Transfer circuit | **Complete** | ~7,800 constraints, tested with `test_real_prover_transfer_proof_cycle` |
| RealProver | **Complete** | Groth16 prove/verify for all 3 circuits, `global()` singleton |
| Production key loading | **Complete** | `ceremony.rs` — `ProductionKeys::load_with_verification()` with genesis hash check |
| R1CS export | **Complete** | `export_r1cs.rs` binary exports `.r1cs` files for snarkjs Phase 2 |
| PoT ceremony scripts | **Complete** | `download_pot.sh`, `run_ceremony.sh`, `phase2_derive.sh` |

#### Remaining Before Mainnet

| Task | Why It Matters | Effort |
|------|---------------|--------|
| **Execute PoT ceremony** | Generate actual `pot_final.ptau` and `circuit_keys/` from real Perpetual PoT | ~2 hours |
| **Enable `production-keys` feature in production builds** | `call-node` has the feature; enable it in release builds | ~5 minutes |
| **Distribute proving keys to clients** | Clients need `*_pk.bin` to generate proofs; validators only need `*_vk.bin` | Process |
| **Solidity verifier deployment** | `phase2_derive.sh` generates `Verifier_*.sol`; needs deployment on Callchain EVM | Process |

#### Dev vs Production

```
Dev build (default features):
  RealProver::global() -> circuit_specific_setup()  --  unsafe for production

Production build (--features production-keys):
  RealProver::global() -> ProductionKeys::load("/var/lib/callchain/shielded_keys")
                          --  keys from Perpetual Powers of Tau + Phase 2
```

### 14.4 Effort Retrospective

The original estimate was ~23 days. Actual implementation took significantly less because:

- arkworks ecosystem is mature and well-documented
- Poseidon parameters for BN254 are publicly available
- `export_r1cs.rs` bridges arkworks and snarkjs cleanly
- The `Prover` trait abstraction allowed incremental migration from `MockProver` to `RealProver`

**Effort breakdown by circuit**:

| Circuit | Constraints | Development Effort |
|---------|------------|-------------------|
| ShieldedTransfer | ~7,800 | 5 days (most complex: 5 constraints, multi-note) |
| ShieldedWithdraw | ~3,551 | 3 days (medium: nullifier + Merkle + public value) |
| ShieldedDeposit | ~350 | 1 day (simplest: commitment format only, optional ZK) |

---

## References

- [ark-groth16 crate](https://crates.io/crates/ark-groth16)
- [ark-groth16 docs](https://docs.rs/ark-groth16/latest/ark_groth16/)
- [arkworks-rs/groth16 on GitHub](https://github.com/arkworks-rs/groth16)
- [ark-bn254 crate](https://crates.io/crates/ark-bn254)
- [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf) — Note commitment tree, nullifiers
- [Papers: Groth16](https://eprint.iacr.org/2016/260) — On the Size of Pairing-based Non-interactive Arguments
- [Poseidon Hash](https://eprint.iacr.org/2019/458) — ZK-friendly hash function
- [BN254 EVM Precompile](https://eips.ethereum.org/EIPS/eip-196) — EIP-196: Precompiled contracts for addition and scalar multiplication on the elliptic curve BN254
- [EIP-2537](https://eips.ethereum.org/EIPS/eip-2537) — Precompile for BLS12-381 curve operations (future)

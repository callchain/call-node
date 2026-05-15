# ZK Shielded Transaction Design for Callchain

**Version**: 0.3.0
**Date**: 2026-05-15
**Spec Reference**: spec.md section 3.8

> **Migration Notice**: This document was originally written for the Groth16/BN254 implementation (v0.2.0). As of 2026-05-15, Callchain has completed migration to Halo2 over Pasta curves. This document reflects the current Halo2 implementation.

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
13. [Completed Migration: Groth16 to Halo2](#13-completed-migration-groth16-to-halo2)
14. [Current Implementation Status](#14-current-implementation-status)

---

## 1. Overview & Threat Model

### 1.1 What Shielded Transactions Provide

Shielded transactions use zk-SNARKs (Halo2) to prove the validity of a transfer without revealing:

- **Sender identity** — which address consumed the input notes
- **Receiver identity** — which address receives the output notes
- **Transfer amount** — the value being transferred (encrypted in the note)
- **Linkability** — which input notes correspond to which output notes

What **is** publicly visible:

- That a shielded transfer occurred (the transaction type)
- The nullifiers of spent notes (prevents double-spend)
- The commitments of new notes (added to Merkle tree)
- The asset ID (which token is being transferred)
- The ZK proof itself (~5-10 KB, Halo2 IPA)

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
value_fp    = bytes_to_fp(value_to_fp_bytes(value))
asset_fp    = bytes_to_fp(asset_id.to_le_bytes() padded to 32 bytes)
rcm_fp      = bytes_to_fp(rcm)
rho_fp      = bytes_to_fp(rho)
commitment  = poseidon_hash([value_fp, asset_fp, rcm_fp, rho_fp])
```

Each component is converted to a Pallas base field element (`Fp`) via `bytes_to_fp`, then hashed with Poseidon. The result is serialized back to a 32-byte `Hash` (B256).

### 2.3 Nullifier Derivation

```
ivk_fp  = bytes_to_fp(recipient_ivk)
fvk_fp  = poseidon_hash_tagged("fvk_from_ivk", [ivk_fp])
nullifier = poseidon_hash_tagged("nullifier", [fvk_fp, rho_fp])
```

The nullifier uniquely identifies a spent note without revealing which note was spent. Domain separation tags (`"fvk_from_ivk"`, `"nullifier"`) ensure the hash output is unique to its purpose and cannot be reused across different contexts.

### 2.4 Random Commitment Mask (RCM) Derivation

The RCM is derived deterministically (not randomly) from the note's components:

```
ivk_fp   = bytes_to_fp(recipient_ivk)
value_fp = bytes_to_fp(value_to_fp_bytes(value))
asset_fp = bytes_to_fp(asset_id.to_le_bytes() padded to 32 bytes)
rho_fp   = bytes_to_fp(rho)
rcm      = poseidon_hash_tagged("rcm", [ivk_fp, value_fp, asset_fp, rho_fp])
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
│  Constraint 1: nullifier[i] = H("nullifier", H("fvk_from_ivk", [ivk]), rho) │
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
  ivk_fr  = bytes_to_fr(notes[i].recipient_ivk)
  fvk_fr  = poseidon_hash_tagged("fvk_from_ivk", [ivk_fr])
  nullifiers[i] == poseidon_hash_tagged("nullifier", [fvk_fr, notes[i].rho_fr])
```

**Halo2 encoding** (with `halo2_gadgets::poseidon::Pow5Chip`):
```rust
// Assign nullifier to instance column (public input)
let nf = meta.query_instance(nf_col, Rotation::cur())?;

// Assign ivk and rho to advice columns (private witnesses)
let ivk = meta.query_advice(ivk_col, Rotation::cur())?;
let rho = meta.query_advice(rho_col, Rotation::cur())?;

// Poseidon hash chip computes fvk and nullifier in-circuit
let fvk = poseidon_chip.hash(layouter, &[ivk])?;
let computed_nf = poseidon_chip.hash(layouter, &[fvk, rho])?;

// Custom gate: nf - computed_nf == 0
meta.create_gate("nullifier_derivation", |meta| {
    vec![(nf - computed_nf) * meta.query_selector(nullifier_sel, Rotation::cur())]
});
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
    left  = if sibling_is_right { current } else { sibling }
    right = if sibling_is_right { sibling } else { current }
    current = poseidon_hash([left, right])
```

**Halo2 encoding** (with Poseidon hash chip):
```rust
// Compute leaf commitment in-circuit
let leaf = poseidon_chip.hash(layouter, &[value, asset, rcm, rho])?;
let mut current = leaf;

for (sibling, is_left) in merkle_path {
    let sib = meta.query_advice(sibling_col, Rotation::cur())?;
    let is_left_bool = meta.query_advice(direction_col, Rotation::cur())?;

    // Conditional swap via custom gate
    let left = is_left_bool * sibling + (1 - is_left_bool) * current;
    let right = is_left_bool * current + (1 - is_left_bool) * sibling;
    current = poseidon_chip.hash(layouter, &[left, right])?;
}

// Public input: expected Merkle root
let root = meta.query_instance(root_col, Rotation::cur())?;
meta.create_gate("merkle_root", |meta| {
    vec![(current - root) * meta.query_selector(merkle_sel, Rotation::cur())]
});
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

**Halo2 encoding**:
```rust
let sk = meta.query_advice(sk_col, Rotation::cur())?;
let derived_ivk = poseidon_chip.hash(layouter, &[sk])?;
let note_ivk = meta.query_advice(ivk_col, Rotation::cur())?;

meta.create_gate("spending_rights", |meta| {
    vec![(derived_ivk - note_ivk) * meta.query_selector(spend_sel, Rotation::cur())]
});
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

**Halo2 encoding**:
```rust
for note in all_notes {
    let val = meta.query_advice(value_col, Rotation::cur())?;

    // Non-zero: val * inv = 1 (inverse exists only if val != 0)
    let inv = meta.query_advice(inv_col, Rotation::cur())?;
    meta.create_gate("non_zero", |meta| {
        vec![(val * inv - 1) * meta.query_selector(nonzero_sel, Rotation::cur())]
    });

    // 128-bit range check via decomposition into 64-bit limbs
    let (hi, lo) = decompose(val, 2, 64);
    range_check::chip.assign(layouter, val, 128)?;
}

for note in output_notes {
    let asset = meta.query_advice(asset_col, Rotation::cur())?;
    let public_asset = meta.query_instance(asset_id_col, Rotation::cur())?;
    meta.create_gate("asset_match", |meta| {
        vec![(asset - public_asset) * meta.query_selector(asset_sel, Rotation::cur())]
    });
}
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

At ~7,800 constraints, this is a medium-sized circuit. Halo2 proving takes ~2-5s on a modern CPU. Verification is ~5-10ms regardless of circuit size.

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
value_var = FpVar::new_witness(cs.clone(), || Ok(value_fr))?
asset_var = FpVar::new_witness(cs.clone(), || Ok(asset_fr))?
rcm_var = FpVar::new_witness(cs.clone(), || Ok(rcm_fr))?
rho_var = FpVar::new_witness(cs.clone(), || Ok(rho_fr))?

// Recompute commitment from components
computed_cm = poseidon_hash_gadget(cs.clone(), &[value_var, asset_var, rcm_var, rho_var])?
public_cm = FpVar::new_input(cs.clone(), || Ok(commitment_fr))?

enforce: computed_cm == public_cm
```

Estimated: ~100 constraints (1 Poseidon hash)

**Constraint D2: Value Range**

```
enforce: value_var > 0
enforce: value_var fits in 128 bits (bit decomposition)
```

Estimated: ~150 constraints (non-zero + 128-bit range check)

**Constraint D3: RCM Determinism**

The RCM must be correctly derived from the note's components (prevents malicious RCM selection):

```
rcm_tag = domain_tag_to_fr("rcm")
rcm_domain_var = poseidon_hash_gadget(cs.clone(), &[rcm_tag, ivk_var, value_var, asset_var, rho_var])?
enforce: rcm_var == rcm_domain_var
```

Estimated: ~100 constraints (1 Poseidon hash with 4 inputs)

**Total constraints**: ~350

**Why Deposit uses a ZK proof**: Deposit is a transparent → shielded transition. The amount is public (deducted from transparent balance), but the ZK proof ensures the note format is correct and the RCM was derived properly — preventing a malicious user from crafting a note with a manipulated commitment that could be used for tracking or double-spend attacks in subsequent transfers.

The `DepositCircuit` (~350 constraints) proves:
- The commitment is the correct Poseidon hash of the note fields
- The value is non-zero and fits in 128 bits
- The RCM was deterministically derived from the note parameters

This is enforced via the same `Halo2Prover::prove_deposit()` path as transfers and withdrawals.

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
│  Constraint W1: nullifier == H("nullifier", H("fvk_from_ivk", [ivk]), rho) │
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
leaf_fr = poseidon_hash_gadget(cs.clone(), &[value_var, asset_var, rcm_var, rho_var])?
current_var = leaf_fr

for (sibling, is_right) in merkle_path:
    sib_var = FpVar::new_witness(cs.clone(), || Ok(bytes_to_fr(sibling)))?
    dir = Boolean::constant(*is_right)
    left_var  = dir.select(&current_var, &sib_var)?
    right_var = dir.select(&sib_var, &current_var)?
    current_var = poseidon_hash_gadget(cs.clone(), &[left_var, right_var])?

root_var = FpVar::new_input(cs.clone(), || Ok(bytes_to_fr(&merkle_root)))?
enforce: current_var == root_var
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

### 5.1 Decision: Pasta Curves (Pallas / Vesta)

Callchain migrated to **Pasta curves** as part of the Halo2 migration:

| Curve | Role | Field Size | Notes |
|-------|------|-----------|-------|
| **Pallas** | Primary circuit curve | 255-bit | Base field = `Fp`; circuits implement `Circuit<Fp>` |
| **Vesta** | Verifier curve | 255-bit | Scalar field = `Fp`; `EqAffine::Scalar = Fp` |

### 5.2 Why Pasta

1. **No Trusted Setup**: Halo2 IPA mode uses universal parameters (`Params::new(k)`), eliminating the need for a Powers of Tau ceremony.

2. **Recursive Composition**: The cycle of curves (Pallas/Vesta) enables future recursive proof aggregation via Nova/Supernova folding.

3. **Orchard-Proven**: `halo2_gadgets` ships with Pasta-optimized Poseidon parameters, battle-tested in Zcash Orchard.

4. **No EVM Precompile Needed**: Verification happens in the native Rust `ShieldedPrecompile` (`0x202`), not via EVM precompiles.

### 5.3 Security Considerations

- Pasta curves provide ~128-bit security (sufficient for payment applications)
- No pairing-friendly requirement means simpler field arithmetic
- Future KZG commitment switch is possible (same circuits, different commitment scheme)

### 5.4 Current Code Alignment

The codebase uses `call-primitives` with:
- `Hash` = B256 (32 bytes) — 32-byte values are split into two Pallas `Fp` elements (128+128 bits) for circuit inputs
- `Address` = 20 bytes (EVM compatible)
- `AssetId` = u64, `Balance` = u128 — both fit within Pallas scalar field (255-bit)

Pallas `Fp` is used for all circuit constraints; Vesta `EqAffine` is used for IPA commitment parameters.

---

## 6. Universal Parameters (No Trusted Setup)

### 6.1 Halo2 IPA: No Ceremony Required

Halo2 in IPA (Inner Product Argument) mode eliminates the trusted setup entirely:

```rust
use halo2_proofs::poly::ipa::commitment::ParamsIPA;
use pasta_curves::vesta::EqAffine;

// Universal parameters are generated deterministically from degree k
let k = 12; // 2^12 = 4096 rows (sufficient for all Callchain circuits)
let params: ParamsIPA<EqAffine> = ParamsIPA::new(k);
```

`Params::new(k)` generates parameters from a deterministic sequence — no secret randomness, no "toxic waste." Anyone can regenerate the same parameters.

### 6.2 Circuit-Specific Keys

From universal parameters, derive circuit-specific proving/verifying keys:

```rust
use halo2_proofs::plonk::{keygen_vk, keygen_pk};

let vk = keygen_vk(&params, &circuit)?;
let pk = keygen_pk(&params, vk.clone(), &circuit)?;
```

- **Proving key** (~tens of KB) — used by prover server to generate proofs
- **Verifying key** (~tens of KB) — embedded in node for verification
- Both are derived from the universal params + circuit definition

### 6.3 Key Rotation

Since there's no ceremony, key rotation is straightforward:

1. Generate new `Params::new(k')` with a higher degree (if circuit grew)
2. Run `keygen_vk`/`keygen_pk` for each circuit
3. Register new keys via governance proposal (`ProposalType::ProverKeyRotation`)
4. Old proofs remain valid during sunset grace period

Governance proposal includes:
- `key_version` (monotonically increasing)
- `transfer_vk_hash`, `deposit_vk_hash`, `withdraw_vk_hash` (SHA-256 of VK bytes)
- `sunset_timestamp` (Unix timestamp when old keys expire)

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
│  3. Verify: Halo2 IPA proof against verifying key                │
│     Inner product argument over polynomial commitments           │
│  4. Check: Value conservation (implicit in proof)                │
│  5. Update: Mark nullifiers spent, insert commitments            │
└──────────────────────────────────────────────────────────────────┘
```

### 7.2 Native Rust Verification (ShieldedPrecompile 0x202)

Halo2 proofs are verified natively in Rust within the `ShieldedPrecompile` at address `0x202`:

```rust
impl Halo2Prover {
    pub fn verify_deposit(
        &self,
        proof_data: &[u8],
        public_inputs: &[Fp],
    ) -> Result<bool, ProverError> {
        let strategy = SingleVerifier::new(&self.deposit_params);
        let mut transcript = Blake2bRead::init(proof_data);

        verify_proof(
            &self.deposit_params,
            &self.deposit_vk,
            strategy,
            &[public_inputs],
            &mut transcript,
        )
        .map_err(|_e| ProverError::ProofVerification)?;

        Ok(true)
    }
}
```

Verification uses the IPA (Inner Product Argument) commitment scheme — no pairing operations required. The verifier checks polynomial evaluations against the structured reference string (SRS) generated by `Params::new(k)`.

### 7.3 Verification Key Storage

Circuit-specific verifying and proving keys are generated at boot:

```
crates/shielded/src/prover.rs:
  Halo2Prover::global():
    - deposit_params  : ParamsIPA<EqAffine>  (universal, ~few KB)
    - deposit_vk      : VerifyingKey<EqAffine>  (circuit-specific, ~tens of KB)
    - deposit_pk      : ProvingKey<EqAffine>    (circuit-specific, ~tens of KB)
    - Same for withdraw and transfer circuits
```

Proving keys are only needed by the prover service (`call-prover`). Validators only need verifying keys.

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
    merkle_tree: PoseidonMerkleTree      // Append-only, depth 32, Poseidon-hashed
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
    ├── poseidon_hash_tagged("call/shielded/ivk", [sk_fr]) ──> incoming_view_key (32 bytes)
    │                                                              │
    │                                                              ├── Can decrypt incoming notes
    │                                                              ├── Used for KYC/whitelist address derivation
    │                                                              └── Given to auditors for read-only access
    │
    └── poseidon_hash_tagged("fvk_from_ivk", [ivk_fr]) ──> full_view_key (32 bytes)
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

KYC and whitelist modes derive a 20-byte address from the note's RCM. This is an **off-chain compliance check** (not part of the ZK circuit), so it uses keccak256 for EVM address compatibility:

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

| Metric | Target | Current (halo2-prover) |
|--------|--------|------------------------|
| Proof generation | 2-5s (client) | 2-5s (halo2_proofs, Pasta) |
| Proof verification | ~5-10ms (node) | ~5-10ms (Halo2 IPA) |
| Proof size | ~5-10KB | ~5-10KB (IPA polynomial commitments) |
| Verifying key | ~tens of KB | ~tens of KB (circuit-specific) |
| Proving key | ~tens of KB | ~tens of KB (circuit-specific) |
| Nullifier check | O(1) | O(1) HashSet + BitSet |
| Merkle tree update | O(log n) | O(log n) Poseidon-hashed |
| Max per block | 50 tx | 50 tx |

### 10.2 Block Time Analysis

With 250ms block time and 50 shielded transactions:
- Total verification: 50 * 5-10ms = 250-500ms
- Exceeds block time at full capacity; 50 tx limit is a hard cap
- Future batch/recursive verification can reduce amortized cost

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

#### Option 2: Server-Side Prover (Implemented)

The `call-prover` crate provides a dedicated HTTP prover service. Clients submit witness data over TLS; the server returns a ZK proof:

```
User wallet ──(encrypted witness)──> call-prover ──(proof)──> call-node verification
```

- Server uses GPU / high-memory hardware, proof time: 3s → 0.3s
- User doesn't wait locally
- Private keys stay on the client; only the witness is sent to the prover
- **Endpoints**: `POST /prove/deposit`, `POST /prove/transfer`, `POST /prove/withdraw`
- **Trade-off**: Requires trusting the prover server availability (witness is sent, but the server cannot spend notes without the spending key)

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
2. Modern CPU Halo2 proving is efficient for medium-sized circuits
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
      │   1. Verify ZK proof (~5-10ms)      │
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

### 11.4 Privacy Analysis of Flow Transitions

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
│  │(PLONKish)│  │ (Halo2)  │  │(encrypt) │  │  (modes)     │    │
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
# crates/shielded/Cargo.toml
halo2_proofs = { version = "0.3", optional = true }  # Halo2 prover/verifier
halo2_gadgets = { version = "0.3", optional = true } # Gadgets (Poseidon, range check)
pasta_curves = { version = "0.5", optional = true }  # Pallas/Vesta curves
ff = { version = "0.13", optional = true }           # Finite field traits
group = { version = "0.13", optional = true }        # Group traits
rand = "0.8"                                         # Random number generation
```

### 12.3 Circuit Implementation

The shielded crate implements **three separate circuits**, each as its own `Circuit<Fp>`:

| Circuit | File | Public Inputs | Private Inputs |
|---------|------|--------------|----------------|
| `DepositCircuit` | `circuit_deposit.rs` | commitment, asset_id | value, rcm, recipient_ivk, rho |
| `WithdrawCircuit` | `circuit_withdraw.rs` | nullifier, asset_id, value, merkle_root | note_value, rcm, recipient_ivk, rho, merkle_path |
| `TransferCircuit` | `circuit_transfer.rs` | asset_id, merkle_root, nullifiers[], commitments[] | input_notes, output_notes, spending_keys, merkle_paths |

**Common patterns across all circuits** (halo2_proofs 0.3):

```rust
use halo2_proofs::circuit::{Chip, Layouter, SimpleFloorPlanner, Value};
use halo2_proofs::plonk::{Advice, Circuit, Column, ConstraintSystem, Error, Instance, Selector};
use halo2_gadgets::poseidon::{Pow5Chip, Pow5Config};
use pasta_curves::pallas::Base as Fp;

// Configure advice columns for witnesses
let value_col = meta.advice_column();
let ivk_col = meta.advice_column();
let rho_col = meta.advice_column();

// Instance column for public inputs
let public_col = meta.instance_column();

// Poseidon chip configuration
let poseidon_config = Pow5Chip::configure(meta, poseidon_advice, poseidon_fixed, poseidon_rc);

// Custom gate for equality constraint
meta.create_gate("nullifier_check", |meta| {
    let nf = meta.query_instance(public_col, Rotation::cur());
    let computed_nf = meta.query_advice(nullifier_col, Rotation::cur());
    vec![(nf - computed_nf) * meta.query_selector(sel, Rotation::cur())]
});

// 128-bit range check via decomposition
let (hi, lo) = decompose(value, 2, 64);
range_check::chip.assign(layouter, value, 128)?;
```

**Transfer circuit excerpt** (nullifier + spending rights + value conservation):

```rust
// --- T1: Nullifier derivation per input ---
for (i, note) in input_notes.iter().enumerate() {
    let nf = meta.query_instance(nf_cols[i], Rotation::cur());
    let ivk = meta.query_advice(ivk_cols[i], Rotation::cur());
    let rho = meta.query_advice(rho_cols[i], Rotation::cur());

    let fvk = poseidon_chip.hash(layouter, &[ivk])?;
    let computed_nf = poseidon_chip.hash(layouter, &[fvk, rho])?;

    meta.create_gate("nullifier", |meta| {
        vec![(nf - computed_nf) * meta.query_selector(nullifier_sel, Rotation::cur())]
    });
}

// --- T2: Merkle path validity per input ---
for (i, note) in input_notes.iter().enumerate() {
    let leaf = poseidon_chip.hash(layouter, &[value, asset, rcm, rho])?;
    let mut current = leaf;
    for (sibling, is_left) in merkle_paths[i].iter() {
        let sib = meta.query_advice(sibling_col, Rotation::cur());
        let left = is_left * sibling + (1 - is_left) * current;
        let right = is_left * current + (1 - is_left) * sibling;
        current = poseidon_chip.hash(layouter, &[left, right])?;
    }
    let root = meta.query_instance(root_col, Rotation::cur());
    meta.create_gate("merkle_root", |meta| {
        vec![(current - root) * meta.query_selector(merkle_sel, Rotation::cur())]
    });
}

// --- T3: Spending rights ---
for (i, note) in input_notes.iter().enumerate() {
    let sk = meta.query_advice(sk_cols[i], Rotation::cur());
    let derived_ivk = poseidon_chip.hash(layouter, &[sk])?;
    let note_ivk = meta.query_advice(ivk_cols[i], Rotation::cur());
    meta.create_gate("spending", |meta| {
        vec![(derived_ivk - note_ivk) * meta.query_selector(spend_sel, Rotation::cur())]
    });
}

// --- T4: Value conservation ---
let mut input_sum = Fp::zero();
for note in &input_notes {
    let val = meta.query_advice(value_col, Rotation::cur());
    input_sum += val;
}
let mut output_sum = Fp::zero();
for note in &output_notes {
    let val = meta.query_advice(value_col, Rotation::cur());
    output_sum += val;
}
let diff = input_sum - output_sum;
let (hi, lo) = decompose(diff, 2, 64);
range_check::chip.assign(layouter, diff, 128)?;
```

### 12.4 Real Prover

```rust
use halo2_proofs::poly::ipa::commitment::ParamsIPA;
use halo2_proofs::plonk::{keygen_vk, keygen_pk, ProvingKey, VerifyingKey};
use pasta_curves::vesta::EqAffine;
use pasta_curves::pallas::Base as Fp;

pub struct Halo2Prover {
    deposit_params: ParamsIPA<EqAffine>,
    deposit_vk: VerifyingKey<EqAffine>,
    deposit_pk: ProvingKey<EqAffine>,
    withdraw_params: ParamsIPA<EqAffine>,
    withdraw_vk: VerifyingKey<EqAffine>,
    withdraw_pk: ProvingKey<EqAffine>,
    transfer_params: ParamsIPA<EqAffine>,
    transfer_vk: VerifyingKey<EqAffine>,
    transfer_pk: ProvingKey<EqAffine>,
}

impl Halo2Prover {
    /// Global singleton (universal params + circuit keys generated at boot).
    pub fn global() -> &'static Self { /* ... */ }

    /// Setup: generate universal params and derive circuit-specific keys.
    pub fn setup() -> Result<Self, ProverError> {
        let deposit_params = ParamsIPA::new(12);
        let deposit_vk = keygen_vk(&deposit_params, &DepositCircuit::default())?;
        let deposit_pk = keygen_pk(&deposit_params, deposit_vk.clone(), &DepositCircuit::default())?;
        // Same for withdraw (k=14) and transfer (k=15)
        ...
    }

    /// Versioned key lookup for verification.
    pub fn for_version(key_version: u32) -> Option<&'static Self> { /* ... */ }

    /// Prove / verify per circuit type (returns ~5-10KB IPA proof bytes).
    pub fn prove_deposit(&self, circuit: &DepositCircuit)    -> Result<Vec<u8>, ProverError>
    pub fn prove_withdraw(&self, circuit: &WithdrawCircuit)  -> Result<Vec<u8>, ProverError>
    pub fn prove_transfer(&self, circuit: &TransferCircuit)  -> Result<Vec<u8>, ProverError>

    pub fn verify_deposit(&self, proof: &[u8], public_inputs: &[Fp]) -> Result<bool, ProverError>
    pub fn verify_withdraw(&self, proof: &[u8], public_inputs: &[Fp]) -> Result<bool, ProverError>
    pub fn verify_transfer(&self, proof: &[u8], public_inputs: &[Fp]) -> Result<bool, ProverError>
}
```

### 12.5 Poseidon Hash Configuration

Poseidon parameters are provided by `halo2_gadgets::poseidon` (Pasta-optimized):

| Parameter | Value | Notes |
|-----------|-------|-------|
| Rate | 2 | 2 field elements absorbed per permutation |
| Width | 3 | 3-element state vector |
| Full rounds | 8 | S-box applied to all state elements |
| Partial rounds | 56 | S-box applied to single element |
| Spec | `P128Pow5T3` | Zcash Orchard parameters for Pasta |

Both the plain hash (`halo2_gadgets::poseidon::primitives::Hash`) and the circuit gadget (`Pow5Chip`) use the same parameters, ensuring the circuit computes the same result as the off-chain node code.

### 12.6 Integration Path

All steps are now complete:

| Step | Description | Status |
|------|-------------|--------|
| 1 | Add `halo2_proofs`, `halo2_gadgets`, `pasta_curves` to `Cargo.toml` | Complete |
| 2 | Replace BN254 Poseidon with Pasta Poseidon (`bytes_to_fp`/`fp_to_bytes`) | Complete |
| 3 | Rewrite `DepositCircuit`, `WithdrawCircuit`, `TransferCircuit` with `Circuit<Fp>` | Complete |
| 4 | Implement `Halo2Prover` with `create_proof`/`verify_proof` for all three circuits | Complete |
| 5 | Update `PoseidonMerkleTree` to use Pasta Poseidon | Complete |
| 6 | Add integration tests that generate and verify real Halo2 proofs | Complete (120+ tests) |
| 7 | Delete `ceremony.rs`, `keygen.rs`, `key_registry.rs` (no trusted setup needed) | Complete |
| 8 | Rewrite `proof_ser.rs` for Halo2 IPA proof serialization (~5-10KB) | Complete |
| 9 | Update precompile verification path (`verify_shielded_proof`) | Complete |
| 10 | Remove `production-keys` feature; `halo2-prover` is the only production path | Complete |

---

## 13. Completed Migration: Groth16 to Halo2

### 13.1 Migration Completed (2026-05-15)

Callchain has completed migration from Groth16/BN254 to Halo2/Pasta. The migration was a breaking protocol change (hard fork) that invalidated all historical shielded state.

| Aspect | Before (Groth16) | After (Halo2) |
|--------|-----------------|---------------|
| Trusted setup | Required (Powers of Tau) | Not required (IPA) |
| Proof size | ~128B | ~5-10KB |
| Verification | ~3ms | ~5-10ms |
| Key generation | Per-circuit ceremony | `Params::new(k)` + `keygen_vk/pk` |
| EVM verification | BN254 `ecPairing` precompile | Native Rust precompile (`0x202`) |
| Arithmetization | R1CS | PLONKish (custom gates + lookup) |

### 13.2 What Changed

1. **Dependencies**: Replaced `ark-groth16`, `ark-bn254`, `ark-r1cs-std` with `halo2_proofs`, `halo2_gadgets`, `pasta_curves`
2. **Circuits**: Rewrote all 3 circuits from R1CS (`ConstraintSynthesizer`) to PLONKish (`Circuit<Fp>`)
3. **Hash function**: Replaced BN254 Poseidon with Pasta Poseidon (`halo2_gadgets::poseidon`)
4. **Proof serialization**: Replaced 128B Groth16 proof with ~5-10KB Halo2 IPA proof
5. **Prover**: `RealProver` → `Halo2Prover`; no trusted setup; `global()` generates params at boot
6. **Key management**: Deleted `ceremony.rs`, `keygen.rs`, `key_registry.rs`; key rotation uses governance proposals with VK hashes

### 13.3 Breaking Changes

- All historical note commitments, nullifiers, and Merkle tree state became invalid
- Protocol version bump required
- Shielded pool started with empty state after migration
- Feature flag renamed: `real-prover` → `halo2-prover` (old flag still aliases for compatibility)

---

## 14. Current Implementation Status

### 14.1 Completed (Zero Stubs)

| Component | File | Status | Tests |
|-----------|------|--------|-------|
| Note structure + encryption | `notes.rs` | Complete | 6 |
| Poseidon hash (plain + gadget) | `poseidon.rs` | Complete | 14 |
| Merkle tree (keccak256, legacy) | `merkle.rs` | Complete | 7 |
| Poseidon Merkle tree (depth 32) | `merkle_poseidon.rs` | Complete | 10 |
| Nullifier set (BitSet) | `nullifiers.rs` | Complete | 5 |
| Circuit constraints (structural) | `circuit.rs` | Complete | 7 |
| Deposit circuit (Halo2) | `circuit_deposit.rs` | Complete | 9 |
| Withdraw circuit (Halo2) | `circuit_withdraw.rs` | Complete | 6 |
| Transfer circuit (Halo2) | `circuit_transfer.rs` | Complete | 8 |
| Halo2Prover | `prover.rs` | Complete | 6 |
| Proof serialization (Halo2 IPA) | `proof_ser.rs` | Complete | 9 |
| Compliance modes (4) | `compliance.rs` | Complete | 12 |
| State machine | `lib.rs` | Complete | 11 |

**Total: 120 passing tests, 15 source files**

### 14.2 Production Readiness Checklist

All ZK components are now using real implementations. No mock or stub code remains in the proving path.

| Component | Implementation | Verified By |
|-----------|---------------|-------------|
| `verify_deposit/withdraw/transfer()` | `verify_proof()` + VK | Unit tests (positive + negative) |
| `prove_deposit/withdraw/transfer()` | `create_proof()` + PK | Real proof cycle tests |
| Merkle tree hash | Poseidon (plain + `Pow5Chip`) | `merkle_poseidon.rs` tests |
| Proof data | ~5-10KB Halo2 IPA proof | `proof_ser.rs` roundtrip tests |
| Nullifier derivation | Poseidon with domain tags | `poseidon.rs` domain tests |
| RCM derivation | Poseidon with domain tags | `circuit_deposit.rs` satisfiability |
| Key hierarchy | Poseidon with domain tags | `circuit_transfer.rs` spending-rights test |

### 14.3 Implementation Status (Updated 2026-04-22)

#### Implemented

| Component | Status | Notes |
|-----------|--------|-------|
| `halo2_proofs` integration | **Complete** | `halo2_proofs`, `halo2_gadgets`, `pasta_curves`, `ff`, `group` all wired |
| Halo2 `Circuit<Fp>` | **Complete** | `DepositCircuit`, `WithdrawCircuit`, `TransferCircuit` with PLONKish constraints |
| Poseidon hash gadget | **Complete** | `poseidon.rs` (Pasta) + `merkle_poseidon.rs` for circuit-friendly hashing |
| Proof serialization | **Complete** | `proof_ser.rs` — Halo2 IPA proof serialization (~5-10KB) |
| Deposit circuit | **Complete** | ~350 constraints, tested with `test_halo2_deposit_proof_cycle` |
| Withdraw circuit | **Complete** | ~3,551 constraints, tested with `test_halo2_withdraw_proof_cycle` |
| Transfer circuit | **Complete** | ~7,800 constraints, tested with `test_halo2_transfer_proof_cycle` |
| Halo2Prover | **Complete** | Halo2 prove/verify for all 3 circuits, `global()` singleton |
| Universal params | **Complete** | `Params::new(k)` — no trusted setup, generated at boot |
| Key rotation governance | **Complete** | `ProposalType::ProverKeyRotation` with VK hash verification |
| Dedicated prover service | **Complete** | `call-prover` crate with HTTP API for remote proof generation |

#### Remaining Before Mainnet

| Task | Why It Matters | Effort |
|------|---------------|--------|
| **Recursive proof aggregation** | Batch-verify multiple Halo2 proofs into one; reduces per-block verification from O(N) to O(1) | 2-3 months |
| **KZG commitment switch** | Smaller proofs (~1KB vs ~5-10KB) at the cost of a universal SRS; same circuits | 2-4 weeks |
| **Compliance lookup circuits** | KYC/whitelist set membership via Halo2 lookup tables | 1-2 months |
| **GPU proving acceleration** | Reduce proof generation time from ~2-5s to sub-second via GPU | 1-2 months |

#### Dev vs Production

```
Dev build (default features):
  Halo2Prover::global() -> Params::new(k) + keygen_vk/pk  --  deterministic, no trusted setup

Production build (--features halo2-prover):
  Same as dev -- Halo2 IPA has no trusted setup requirement
  Key rotation is governance-driven via ProverKeyRotation proposals
```

### 14.4 Effort Retrospective

The migration from Groth16 to Halo2 took ~6 weeks across 6 phases:

- Phase 1: Dependencies + Pasta Poseidon foundation
- Phase 2: Deposit circuit (simplest, validate approach)
- Phase 3: Withdraw circuit
- Phase 4: Transfer circuit (most complex)
- Phase 5: Prover/Verifier/Serialization rewrite
- Phase 6: Cleanup (delete obsolete files, update docs, full test suite)

**Effort breakdown by circuit**:

| Circuit | Constraints | Development Effort |
|---------|------------|-------------------|
| ShieldedTransfer | ~7,800 | 5 days (most complex: 5 constraints, multi-note) |
| ShieldedWithdraw | ~3,551 | 3 days (medium: nullifier + Merkle + public value) |
| ShieldedDeposit | ~350 | 1 day (simplest: commitment + range + RCM determinism) |

---

## References

- [halo2_proofs crate](https://crates.io/crates/halo2_proofs)
- [halo2_gadgets crate](https://crates.io/crates/halo2_gadgets)
- [pasta_curves crate](https://crates.io/crates/pasta_curves)
- [zcash/halo2 on GitHub](https://github.com/zcash/halo2)
- [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf) — Note commitment tree, nullifiers
- [Zcash Orchard](https://zips.z.cash/protocol/protocol.pdf#orchard) — Halo2 circuits in production
- [Poseidon Hash](https://eprint.iacr.org/2019/458) — ZK-friendly hash function
- [Papers: Halo](https://eprint.iacr.org/2019/1021) — Recursive Proof Composition without a Trusted Setup

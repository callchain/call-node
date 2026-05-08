# Formal Verification Specification for Callchain Shielded Circuits

> **Status**: In Progress — Lean 4 formalization underway
> **Scope**: DepositCircuit, TransferCircuit, WithdrawCircuit
> **Target**: Theorem-prover proofs of completeness and soundness

---

## 1. Overview

This document defines the mathematical model for the Callchain shielded pool ZK circuits. It serves as the bridge between the Rust implementation (`crates/shielded/src/circuit_*.rs`) and the Lean 4 formal proofs.

**Circuits under verification**:

| Circuit | File | Public Inputs | Private Witnesses | Constraints |
|---------|------|--------------|-------------------|-------------|
| Deposit | `circuit_deposit.rs` | commitment, asset_id | value, rcm, ivk, rho | D1–D3 |
| Transfer | `circuit_transfer.rs` | nullifiers[], commitments[], asset_id, merkle_root | input_notes[], output_notes[], merkle_paths[] | T1–T5 |
| Withdraw | `circuit_withdraw.rs` | nullifier, asset_id, value, target_address, merkle_root | note_value, rcm, recipient_ivk, rho, merkle_path | W1–W4 |

**Completeness**: Every valid high-level witness satisfies the R1CS constraints.
**Soundness**: Every R1CS-satisfying assignment encodes a valid high-level witness.

---

## 2. Finite Field: BN254 Fr

The circuits operate over the scalar field of the BN254 elliptic curve.

### 2.1 Prime Definition

```
p = 21888242871839275222246405745257275088548364400416034343698204186575808495617
```

The field is `Fr = Fp` where `p` is the 254-bit prime above.

### 2.2 Properties

- `Fr` is a prime field of characteristic `p`
- The multiplicative group `Fr*` has order `p - 1`
- Every non-zero element has a multiplicative inverse
- Zero element: `0`
- One element: `1`

### 2.3 Byte Encoding

Field elements are encoded as 32-byte little-endian arrays. The mapping `bytes_to_fr: [0, 32) -> Fr` is defined as:

```
bytes_to_fr(b[0..32]) = Σ b[i] * 256^i  (mod p)
```

Note: This is a lossy mapping if the byte value exceeds `p`. In practice, witnesses are generated such that the resulting field element is canonical.

### 2.4 Value Encoding

A `u128` value `v` is encoded to 32 bytes as:

```
value_to_fr_bytes(v) = LE_32(v) = [v_0, v_1, ..., v_15, 0, ..., 0]
```

where `v_i` are the little-endian bytes of `v` and the remaining 16 bytes are zero.

---

## 3. Poseidon Hash Function

The circuits use the Poseidon hash over BN254 Fr with parameters from `poseidon-ark-no-std`.

### 3.1 Parameters

| Parameter | Value |
|-----------|-------|
| Rate | 8 (maximum 16 inputs) |
| Full rounds `R_F` | 8 |
| Partial rounds `R_P` | Size-dependent: `R_P[t-2]` where `t = inputs.len() + 1` |
| S-box | `x^5` (quintic) |
| MDS matrix | `m[t-2][i][j]` from `poseidon-ark-no-std` |
| Round constants | `c[t-2][round * t + i]` from `poseidon-ark-no-std` |

The partial round counts `R_P` for each width `t`:

| t | R_P |
|---|-----|
| 2 | 56 |
| 3 | 57 |
| 4 | 56 |
| 5 | 60 |
| 6 | 60 |
| 7 | 63 |
| 8 | 64 |
| 9 | 63 |
| 10 | 66 |
| 11 | 65 |
| 12 | 70 |
| 13 | 68 |
| 14 | 70 |
| 15 | 70 |
| 16 | 70 |
| 17 | 70 |

### 3.2 Permutation

The Poseidon permutation `perm(state, t, R_F, R_P)` operates on a state vector of `t` field elements:

```
Initial state: [capacity=0, input_1, input_2, ..., input_{t-1}]

For round = 0 to R_F + R_P - 1:
    // Ark (Add Round Constants)
    For i = 0 to t-1:
        state[i] = state[i] + rc[round * t + i]

    // S-Box (x^5)
    If round < R_F/2 or round >= R_F/2 + R_P:
        // Full round: apply S-box to all elements
        For i = 0 to t-1:
            state[i] = state[i]^5
    Else:
        // Partial round: apply S-box to first element only
        state[0] = state[0]^5

    // Mix (MDS matrix multiplication)
    new_state[i] = Σ_{j=0}^{t-1} m[i][j] * state[j]   for all i
    state = new_state

Output: state[0]
```

### 3.3 Hash Function

```
poseidon_hash(inputs) = perm([0, inputs[0], inputs[1], ...], t, R_F, R_P)
where t = len(inputs) + 1
```

Preconditions: `1 <= len(inputs) <= 16`

### 3.4 Domain-Tagged Hash

```
poseidon_hash_tagged(tag, inputs) = poseidon_hash([tag_fr, inputs[0], inputs[1], ...])
where tag_fr = bytes_to_fr(tag_as_utf8_bytes)
```

Domain tags used in the circuits:

| Tag | String | Purpose |
|-----|--------|---------|
| `IVK_FROM_SK` | `"call/shielded/ivk"` | Derive incoming viewing key from spending key |
| `FVK_FROM_IVK` | `"fvk_from_ivk"` | Derive full viewing key from incoming viewing key |
| `NULLIFIER` | `"nullifier"` | Derive nullifier |
| `RCM` | `"rcm"` | Derive random commitment material |
| `COMMITMENT` | `"commitment"` | Derive note commitment |
| `MERKLE` | `"merkle"` | Merkle tree hashing |

### 3.5 Poseidon in R1CS

The circuit gadget `poseidon_hash_gadget` enforces the same computation using:
- Witness variables for inputs
- Constant variables for round constants and MDS entries
- Multiplication constraints for `x^5 = x^2 * x^2 * x`
- Addition constraints for Ark and Mix steps

The gadget generates `O(R_F * t + R_P)` constraints.

---

## 4. R1CS Constraint System

### 4.1 Definition

A Rank-1 Constraint System (R1CS) over `Fr` is a collection of constraints of the form:

```
(A · w) * (B · w) = (C · w)
```

where `w = [1, public_inputs..., private_witness...]` is the witness vector, and `A, B, C` are coefficient vectors.

### 4.2 Satisfaction Predicate

An assignment `w` satisfies an R1CS if for every constraint `i`:

```
(Σ_j A[i][j] * w[j]) * (Σ_j B[i][j] * w[j]) = Σ_j C[i][j] * w[j]
```

### 4.3 arkworks Mapping

In the Rust implementation:
- `FpVar::new_input` creates public input variables
- `FpVar::new_witness` creates private witness variables
- `FpVar::new_constant` creates constant variables
- `enforce_equal` generates R1CS constraints
- Multiplications of `FpVar` generate multiplication constraints

---

## 5. High-Level Validity Predicates

### 5.1 DepositCircuit Validity

A deposit witness `w = (value, rcm, ivk, rho)` is **valid** for public inputs `(commitment, asset_id)` iff:

```
D-Valid-1: value > 0                           (non-zero)
D-Valid-2: value < 2^128                       (128-bit range)
D-Valid-3: commitment = H(value, asset_id, rcm, rho)
D-Valid-4: rcm = H_tagged("rcm", ivk, value, asset_id, rho)
```

where `H` is `poseidon_hash` and `H_tagged` is `poseidon_hash_tagged`.

### 5.2 TransferCircuit Validity

A transfer witness `w = (inputs, outputs, merkle_paths)` is **valid** for public inputs `(nullifiers, commitments, asset_id, merkle_root)` iff:

```
T-Valid-1: len(inputs) == len(nullifiers) == len(merkle_paths) >= 1
T-Valid-2: len(outputs) == len(commitments) >= 1
T-Valid-3: For each input i:
    T-Valid-3a: input_i.value > 0
    T-Valid-3b: input_i.value < 2^128
    T-Valid-3c: nullifier_i = H(H_tagged("fvk_from_ivk", input_i.ivk), input_i.rho)
    T-Valid-3d: input_i.ivk = H_tagged("call/shielded/ivk", input_i.spending_key)
    T-Valid-3e: merkle_root = MerkleRoot(H(input_i.value, asset_id, input_i.rcm, input_i.rho), merkle_path_i)
T-Valid-4: For each output j:
    T-Valid-4a: output_j.value > 0
    T-Valid-4b: output_j.value < 2^128
    T-Valid-4c: commitment_j = H(output_j.value, asset_id, output_j.rcm, output_j.rho)
T-Valid-5: Σ output_values <= Σ input_values
```

### 5.3 WithdrawCircuit Validity

A withdraw witness `w = (note_value, rcm, recipient_ivk, rho, merkle_path)` is **valid** for public inputs `(nullifier, asset_id, value, target_address, merkle_root)` iff:

```
W-Valid-1: note_value > 0
W-Valid-2: note_value < 2^128
W-Valid-3: note_value == value
W-Valid-4: nullifier = H(H_tagged("fvk_from_ivk", recipient_ivk), rho)
W-Valid-5: merkle_root = MerkleRoot(H(note_value, asset_id, rcm, rho), merkle_path)
```

### 5.4 Merkle Root Function

```
MerkleRoot(leaf, path):
    current = leaf
    for (sibling, is_right) in path:
        if is_right:
            current = H(current, sibling)
        else:
            current = H(sibling, current)
    return current
```

---

## 6. Completeness Theorems

### 6.1 DepositCircuit Completeness

```
Theorem DepositCompleteness:
  For all witnesses w = (value, rcm, ivk, rho),
  For all public inputs (commitment, asset_id),
  If ValidDeposit(w, commitment, asset_id),
  Then R1CS_Satisfied(DepositCircuit(commitment, asset_id), Encode(w))
```

**Proof sketch**: By construction, each constraint in `DepositCircuit::generate_constraints` exactly mirrors one of the validity conditions D-Valid-3, D-Valid-4, or the range/non-zero checks. A valid witness provides values that make each equality hold, and the range/non-zero constraints are satisfied by D-Valid-1 and D-Valid-2.

### 6.2 TransferCircuit Completeness

```
Theorem TransferCompleteness:
  For all witnesses w = (inputs, outputs, merkle_paths),
  For all public inputs (nullifiers, commitments, asset_id, merkle_root),
  If ValidTransfer(w, nullifiers, commitments, asset_id, merkle_root),
  Then R1CS_Satisfied(TransferCircuit(...), Encode(w))
```

**Proof sketch**: Each input note contributes constraints for nullifier derivation (T-Valid-3c), spending rights (T-Valid-3d), Merkle path (T-Valid-3e), and range checks (T-Valid-3a, T-Valid-3b). Each output note contributes commitment constraints (T-Valid-4c) and range checks (T-Valid-4a, T-Valid-4b). The value conservation constraint enforces T-Valid-5. A valid witness satisfies all of these by definition.

### 6.3 WithdrawCircuit Completeness

```
Theorem WithdrawCompleteness:
  For all witnesses w = (note_value, rcm, recipient_ivk, rho, merkle_path),
  For all public inputs (nullifier, asset_id, value, target_address, merkle_root),
  If ValidWithdraw(w, nullifier, asset_id, value, target_address, merkle_root),
  Then R1CS_Satisfied(WithdrawCircuit(...), Encode(w))
```

**Proof sketch**: Similar to Transfer but with a single input and no outputs. The value match constraint enforces W-Valid-3.

---

## 7. Soundness Theorems

### 7.1 DepositCircuit Soundness

```
Theorem DepositSoundness:
  For all assignments a,
  For all public inputs (commitment, asset_id),
  If R1CS_Satisfied(DepositCircuit(commitment, asset_id), a),
  Then there exists a witness w such that:
    ValidDeposit(w, commitment, asset_id)
    AND Encode(w) = a
```

**Proof sketch**:
1. From the R1CS satisfaction, decode the assignment into `(value, rcm, ivk, rho)`.
2. Constraint D1 (commitment validity) ensures `commitment = H(value, asset_id, rcm, rho)`, which is D-Valid-3.
3. Constraint D3 (RCM determinism) ensures `rcm = H_tagged("rcm", ivk, value, asset_id, rho)`, which is D-Valid-4.
4. The non-zero constraint (inverse witness) ensures `value != 0`, which is D-Valid-1.
5. The 128-bit range constraint ensures `value < 2^128`, which is D-Valid-2.

### 7.2 TransferCircuit Soundness

```
Theorem TransferSoundness:
  For all assignments a,
  For all public inputs (nullifiers, commitments, asset_id, merkle_root),
  If R1CS_Satisfied(TransferCircuit(...), a),
  Then there exists a witness w such that:
    ValidTransfer(w, nullifiers, commitments, asset_id, merkle_root)
    AND Encode(w) = a
```

**Key lemmas needed**:
- **Merkle path soundness**: If the Merkle path constraints are satisfied, the computed root equals the public root, proving the note commitment exists in the tree.
- **Value conservation soundness**: The constraint `diff = input_sum - output_sum` with `diff >= 0` (enforced by 128-bit range on diff) ensures `sum(outputs) <= sum(inputs)`.
- **Spending rights soundness**: The constraint `ivk = H_tagged("call/shielded/ivk", sk)` ensures the prover knows the spending key corresponding to the note's IVK.

### 7.3 WithdrawCircuit Soundness

```
Theorem WithdrawSoundness:
  For all assignments a,
  For all public inputs (nullifier, asset_id, value, target_address, merkle_root),
  If R1CS_Satisfied(WithdrawCircuit(...), a),
  Then there exists a witness w such that:
    ValidWithdraw(w, nullifier, asset_id, value, target_address, merkle_root)
    AND Encode(w) = a
```

**Proof sketch**: Similar to TransferCircuit soundness but simpler (single input, no outputs). The value match constraint directly enforces W-Valid-3.

---

## 8. Correspondence with Rust Implementation

### 8.1 Constraint Mapping: DepositCircuit

| Rust Constraint | R1CS Form | Validity Condition |
|-----------------|-----------|-------------------|
| `computed_cm.enforce_equal(&commitment_var)` | `H(v, a, r, ρ) = cm` | D-Valid-3 |
| `value * value_inv = 1` | `v ≠ 0` | D-Valid-1 |
| `bits[128..] = 0` | `v < 2^128` | D-Valid-2 |
| `computed_rcm.enforce_equal(&rcm_var)` | `H_tag("rcm", ivk, v, a, ρ) = rcm` | D-Valid-4 |

### 8.2 Constraint Mapping: TransferCircuit

| Rust Constraint | R1CS Form | Validity Condition |
|-----------------|-----------|-------------------|
| `computed_nf.enforce_equal(&nullifier_vars[i])` | `H(H_tag("fvk_from_ivk", ivk), ρ) = nf` | T-Valid-3c |
| `derived_ivk.enforce_equal(&ivk_var)` | `H_tag("call/shielded/ivk", sk) = ivk` | T-Valid-3d |
| `current.enforce_equal(&merkle_root_var)` | `MerkleRoot(cm, path) = root` | T-Valid-3e |
| `computed_cm.enforce_equal(&commitment_vars[j])` | `H(v, a, r, ρ) = cm` | T-Valid-4c |
| `diff + output_sum = input_sum` | `Σ out <= Σ in` | T-Valid-5 |
| `value * inv = 1` (each note) | `v > 0` | T-Valid-3a, T-Valid-4a |
| `bits[128..] = 0` (each note) | `v < 2^128` | T-Valid-3b, T-Valid-4b |

### 8.3 Constraint Mapping: WithdrawCircuit

| Rust Constraint | R1CS Form | Validity Condition |
|-----------------|-----------|-------------------|
| `computed_nf.enforce_equal(&nullifier_var)` | `H(H_tag("fvk_from_ivk", ivk), ρ) = nf` | W-Valid-4 |
| `current.enforce_equal(&merkle_root_var)` | `MerkleRoot(cm, path) = root` | W-Valid-5 |
| `value_var.enforce_equal(&public_value_var)` | `note_value = value` | W-Valid-3 |
| `value * inv = 1` | `v > 0` | W-Valid-1 |
| `bits[128..] = 0` | `v < 2^128` | W-Valid-2 |

---

## 9. Lean 4 Formalization Strategy

### 9.1 Proof Architecture

```
Fr.lean          →  BN254 field definition and properties
Poseidon.lean    →  Poseidon hash function model
R1CS.lean        →  Generic R1CS satisfaction predicate
DepositCircuit.lean  →  Deposit constraints + completeness/soundness
TransferCircuit.lean →  Transfer constraints + completeness/soundness (future)
WithdrawCircuit.lean →  Withdraw constraints + completeness/soundness (future)
```

### 9.2 Proof Tactics

- `simp` for unfolding definitions
- `field_simp` for field arithmetic simplifications
- `rw [poseidon_hash_def]` for hash function properties
- `omega` / `linarith` for integer inequalities (range checks)
- `use` for existential introduction (soundness proofs)
- `intro` / `assume` for implication introduction

### 9.3 Poseidon Constants Extraction

The Poseidon MDS matrices and round constants from `poseidon-ark-no-std` must be extracted into Lean `def`s. A Rust script (`scripts/extract_poseidon_constants.rs`) will read the crate's constant data and emit Lean code.

---

## 10. References

- [Poseidon Paper](https://eprint.iacr.org/2019/458.pdf) — Grassi et al.
- [arkworks R1CS](https://docs.rs/ark-relations/latest/ark_relations/r1cs/) — Rust constraint system framework
- [poseidon-ark-no-std](https://github.com/arnaucube/poseidon-ark-no-std) — Constants source
- [mathlib4](https://github.com/leanprover-community/mathlib4) — Lean 4 mathematical library
- [Zcash Sapling Formal Verification](https://github.com/zcash/zcash/issues/3412) — Prior art in ZK circuit verification

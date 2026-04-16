# ZK Shielded Transaction: Real Prover Implementation

## Context

The `call-shielded` crate has 55 passing tests covering note encryption, Merkle tree, nullifier set, constraint checks, compliance modes, and state machine — but the **ZK proving layer is entirely mocked**. Both `MockProver` and `Groth16Prover` return fixed 200-byte dummy proofs. No arkworks dependencies exist.

**Goal**: Replace mock prover with real Groth16 prover using arkworks + BN254, implementing all 3 circuits (Deposit, Transfer, Withdraw) as specified in `zk.md`.

## Current State (from `crates/shielded/`)

| File | Status | What's Real | What's Mock |
|------|--------|-------------|-------------|
| `notes.rs` | Complete | Note struct, commitment/nullifier derivation, ChaCha20-Poly1305 | — |
| `merkle.rs` | Complete | Incremental tree (depth 32, keccak256), proofs | — |
| `nullifiers.rs` | Complete | HashSet + BitSet compression | — |
| `circuit.rs` | Partial | 5 constraint checks in plaintext | Not in R1CS circuit |
| `prover.rs` | Mock | `Prover` trait interface | Both impls return dummy bytes |
| `compliance.rs` | Complete | 4 modes, AuditRecord | — |
| `lib.rs` | Partial | ShieldedState, ZkProof struct | `verify_zk_proof()` structural only |
| `Cargo.toml` | Gap | — | No arkworks deps |

## Files That Need Changes (outside shielded crate)

| File | Change |
|------|--------|
| `crates/protocol/src/instructions.rs` | Add `nullifiers`, `commitments`, `encrypted_notes` to shielded instruction variants |
| `crates/protocol/src/instructions.rs` (line 285-296) | Wire `ShieldedTransfer` to actually call `ShieldedState.process_transfer()` |

---

## [ ] Phase 1: arkworks Dependencies — Not Implemented

**File**: `crates/shielded/Cargo.toml`

Add:
```toml
ark-std = "0.5"
ark-ff = "0.5"
ark-ec = "0.5"
ark-groth16 = "0.5"
ark-r1cs-std = "0.5"
ark-bn254 = "0.5"
ark-relations = "0.5"
ark-serialize = "0.5"
poseidon-ark = "0.0.1"       # Poseidon hash gadget for BN254
```

---

## [ ] Phase 2: Poseidon Hash Module — Not Implemented

**New file**: `crates/shielded/src/poseidon.rs`

- Poseidon hash wrapper for BN254 Fr field
- Parameters: rate=8, capacity=4, full_rounds=8, partial_rounds=57
- Two modes:
  - **Plain Rust** (`poseidon_hash(inputs) -> [u8; 32]`): used by nodes for commitment/nullifier derivation outside the circuit
  - **Circuit gadget** (`poseidon_hash_gadget(&[FpVar<Fr>]) -> FpVar<Fr>`): used inside R1CS constraints
- Domain-separated hashes: `"fvk_from_ivk"`, `"nullifier"`, `"rcm"`, `"commitment"` tags
- Keep existing keccak256 tree in `merkle.rs` (used for on-chain storage), add parallel Poseidon tree for circuit proofs

Estimated: ~300 lines, ~15 tests

---

## [ ] Phase 3: Poseidon Merkle Tree — Not Implemented

**New file**: `crates/shielded/src/merkle_poseidon.rs` (or extend `merkle.rs`)

- Incremental Merkle tree using Poseidon hash instead of keccak256
- Same API as existing `IncrementalMerkleTree`: `insert()`, `proof_for_index()`, `root()`
- Default depth: 32 (can optimize to 20 later per zk.md Option 1)
- `verify_merkle_path_poseidon()` standalone verifier
- Shared between Transfer and Withdraw circuits

Estimated: ~150 lines, ~10 tests

---

## [ ] Phase 4: ShieldedTransfer Circuit (R1CS ConstraintSynthesizer) — Not Implemented

**New file**: `crates/shielded/src/circuit_transfer.rs`

Implement `ConstraintSynthesizer<Fr>` for BN254:

```rust
pub struct TransferCircuit {
    // Public inputs
    pub nullifiers: Vec<[u8; 32]>,    // N inputs
    pub commitments: Vec<[u8; 32]>,   // M outputs
    pub asset_id: u64,
    pub merkle_root: [u8; 32],

    // Private witnesses
    pub input_notes: Vec<NoteWitness>,       // (value, rcm, ivk, rho)
    pub output_notes: Vec<NoteWitness>,
    pub merkle_paths: Vec<Vec<([u8; 32], bool)>>,
    pub spending_keys: Vec<[u8; 32]>,
}
```

**5 constraints in R1CS** (per zk.md §4):

| Constraint | Method | Estimated |
|------------|--------|-----------|
| 1. Nullifier derivation | `poseidon_hash(ivk) -> fvk`, `poseidon_hash(fvk, rho) -> nf`, enforce == public | ~200 per input |
| 2. Merkle path validity | Walk path with `poseidon_hash`, enforce root == public | ~3200 per input |
| 3. Spending rights | IVK derived from spending_key matches note IVK | ~100 per input |
| 4. Value conservation | Sum outputs <= sum inputs, enforce non-negative diff | ~50 per note |
| 5. Range & asset validity | Non-zero check, 128-bit range, asset_id match | ~150 per note |

Total for 2-in/2-out: ~7,800 constraints

**Helper functions**:
- `enforce_non_zero(cs, val)` — inverse check
- `enforce_128_bit_range(cs, val)` — bit decomposition
- `enforce_non_negative(cs, diff)` — range check on difference
- `compute_note_commitment_gadget(note, cs)` — Poseidon hash of note fields
- `cond_select(is_right, left, right)` — circuit conditional select

Estimated: ~800 lines, ~8 tests

---

## [ ] Phase 5: ShieldedWithdraw Circuit — Not Implemented

**New file**: `crates/shielded/src/circuit_withdraw.rs`

Subset of Transfer circuit:

```rust
pub struct WithdrawCircuit {
    // Public inputs
    pub nullifier: [u8; 32],
    pub asset_id: u64,
    pub value: u128,              // Public — credited to transparent balance
    pub target_address: [u8; 20],
    pub merkle_root: [u8; 32],

    // Private witnesses
    pub note_value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
    pub merkle_path: Vec<([u8; 32], bool)>,
}
```

**4 constraints**:
| Constraint | Notes |
|------------|-------|
| W1. Nullifier derivation | Same as Transfer (1 input) |
| W2. Merkle path validity | Same as Transfer (1 input) |
| W3. Value match | Enforce note_value == public_value |
| W4. Range & asset | Non-zero, 128-bit, asset match |

Total: ~3,551 constraints

Estimated: ~400 lines, ~6 tests

---

## [ ] Phase 6: ShieldedDeposit Circuit — Not Implemented

**New file**: `crates/shielded/src/circuit_deposit.rs`

Simplest circuit:

```rust
pub struct DepositCircuit {
    // Public inputs
    pub commitment: [u8; 32],
    pub asset_id: u64,

    // Private witnesses
    pub value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
}
```

**3 constraints**:
| Constraint | Notes |
|------------|-------|
| D1. Commitment validity | Recompute H(value || asset_id || rcm || rho) == public |
| D2. Value range | Non-zero, 128-bit |
| D3. RCM determinism | H("rcm" || ivk || value || asset_id || rho) == rcm |

Total: ~350 constraints

Estimated: ~200 lines, ~5 tests

---

## [ ] Phase 7: RealProver Implementation — Not Implemented

**File**: `crates/shielded/src/prover.rs` — replace stub implementations

```rust
pub struct RealProver {
    transfer_pk: ProvingKey<Bn254>,
    transfer_vk: VerifyingKey<Bn254>,
    withdraw_pk: ProvingKey<Bn254>,
    withdraw_vk: VerifyingKey<Bn254>,
    deposit_pk: ProvingKey<Bn254>,
    deposit_vk: VerifyingKey<Bn254>,
}
```

**Methods**:
- `setup()` — generates dev/test keys via `Groth16::circuit_specific_setup()`
- `load_keys(paths)` — loads from disk (production use)
- `prove_transfer(circuit) -> ZkProof` — calls `Groth16::prove()`, serializes G1/G2 points
- `prove_withdraw(circuit) -> ZkProof`
- `prove_deposit(circuit) -> ZkProof`
- `verify_transfer(proof) -> bool` — calls `Groth16::verify_with_processed_vk()`
- `verify_withdraw(proof) -> bool`
- `verify_deposit(proof) -> bool`

**Proof serialization** (new module `crates/shielded/src/proof_ser.rs`):
- `serialize_groth16_proof(proof: ark_groth16::Proof<Bn254>) -> Vec<u8>`
- `deserialize_groth16_proof(data: &[u8]) -> Result<ark_groth16::Proof<Bn254>>`
- Format: compressed G1 (32B) + compressed G2 (64B) + compressed G1 (32B) = 128B
- Plus public inputs appended

Estimated: ~500 lines prover + ~200 lines serialization, ~12 tests

---

## [ ] Phase 8: Wire into lib.rs — Not Implemented

**File**: `crates/shielded/src/lib.rs`

- Update `verify_zk_proof()` to use `RealProver` instead of structural check
- Update `ShieldedState.process_transfer()` to call real verification
- Add `ShieldedState.process_deposit()` and `ShieldedState.process_withdraw()`
- Add `RealProver` field to `ShieldedState` (optional, for node-side verification)
- Feature flag: `real-prover` (default off, keeps mock for CI speed)

**File**: `crates/protocol/src/instructions.rs` (line 285-296)

Wire shielded instructions to actually call shielded state:
```rust
Instruction::ShieldedTransfer { asset_id, nullifiers, commitments, proof, .. } => {
    let transfer = ShieldedTransfer {
        input_notes: /* derive from commitment data */,
        output_notes: /* derive from commitment data */,
        proof: ZkProof { proof_data: proof.clone(), nullifiers: ..., commitments: ..., asset_id: *asset_id },
    };
    shielded_state.process_transfer(&transfer)?;
    Ok(InstructionResult::Success)
}
```

Need to extend instruction variants to carry `nullifiers`, `commitments`, and `encrypted_notes` fields.

---

## [ ] Phase 9: Integration Tests — Not Implemented

**File**: `crates/protocol/tests/test_shielded_flow.rs` (extend existing)

Add real prover tests:
- `test_real_deposit_proof()` — generate real Groth16 proof for deposit
- `test_real_transfer_proof()` — 2-in/2-out transfer with real proof
- `test_real_withdraw_proof()` — withdraw to transparent address
- `test_real_proof_verify_valid()` — valid proof passes verification
- `test_real_proof_reject_tampered()` — tampered proof fails
- `test_real_proof_reject_double_spend()` — reuse nullifier rejected
- `test_real_deposit_then_transfer_flow()` — deposit → transfer → withdraw end-to-end

Also add a new benchmark test:
- `test_proving_time()` — measure proof generation time, assert < 5s

---

## [ ] Phase 10: CRS Generation (Dev/Testing) — Not Implemented

**New file**: `crates/shielded/src/keygen.rs`

- Dev keygen: `Groth16::circuit_specific_setup()` per circuit
- Key serialization: save/load PK/VK to disk
- Key info metadata: constraint count, public input count
- For production: document Powers of Tau ceremony steps (not implemented yet)

Estimated: ~100 lines, ~4 tests

---

## [ ] Phase 11: Unit Tests — Not Implemented

**File**: `crates/shielded/src/poseidon.rs` (new tests, ~15)
- `test_poseidon_hash_deterministic()` — same inputs always produce same output
- `test_poseidon_hash_different_inputs()` — single bit change produces different output
- `test_poseidon_hash_domain_separation()` — "nullifier" vs "rcm" tags produce different hashes
- `test_poseidon_hash_known_vector()` — against reference Poseidon implementation
- `test_poseidon_gadget_single_input()` — circuit gadget with 1 input
- `test_poseidon_gadget_two_inputs()` — circuit gadget with 2 inputs
- `test_poseidon_gadget_five_inputs()` — circuit gadget with 5 inputs
- `test_poseidon_gadget_deterministic()` — gadget matches plain hash
- `test_poseidon_hash_empty_input()` — empty input produces valid output
- `test_poseidon_hash_large_input()` — maximum input count (rate=8)
- `test_poseidon_gadget_constraints_satisfied()` — verify constraint system is valid
- `test_poseidon_gadget_constraint_count()` — verify per-hash constraint cost
- `test_poseidon_hash_bn254_field_element()` — output is valid Fr element
- `test_poseidon_hash_serialization()` — round-trip serialize/deserialize
- `test_poseidon_config_defaults()` — config parameters match zk.md spec

**File**: `crates/shielded/src/merkle_poseidon.rs` (new tests, ~10)
- `test_poseidon_merkle_insert_single()` — single leaf, root equals leaf hash
- `test_poseidon_merkle_insert_multiple()` — multiple leaves, deterministic root
- `test_poseidon_merkle_proof_last()` — proof for most recent leaf
- `test_poseidon_merkle_proof_index()` — proof for arbitrary index
- `test_poseidon_merkle_verify_valid()` — valid proof passes verification
- `test_poseidon_merkle_verify_invalid_path()` — wrong sibling rejected
- `test_poseidon_merkle_verify_wrong_index()` — wrong index bit rejected
- `test_poseidon_merkle_depth_32()` — full depth tree insertion
- `test_poseidon_merkle_empty_tree()` — empty tree has known root
- `test_poseidon_merkle_root_deterministic()` — same inserts produce same root

**File**: `crates/shielded/src/circuit_deposit.rs` (new tests, ~8)
- `test_deposit_circuit_satisfiable()` — valid witness satisfies constraints
- `test_deposit_circuit_commitment_validity()` — commitment matches note components
- `test_deposit_circuit_value_range()` — zero value rejected
- `test_deposit_circuit_rcm_determinism()` — RCM must match derived value
- `test_deposit_circuit_wrong_commitment()` — mismatched public commitment rejected
- `test_deposit_circuit_asset_id_mismatch()` — wrong asset_id rejected
- `test_deposit_circuit_value_overflow()` — value > u128 max rejected
- `test_deposit_circuit_constraint_count()` — verify ~350 constraints

**File**: `crates/shielded/src/circuit_withdraw.rs` (new tests, ~10)
- `test_withdraw_circuit_satisfiable()` — valid witness satisfies constraints
- `test_withdraw_circuit_nullifier_derivation()` — nullifier matches IVK + rho
- `test_withdraw_circuit_merkle_path_valid()` — proof of note existence in tree
- `test_withdraw_circuit_value_match()` — private value equals public value
- `test_withdraw_circuit_wrong_nullifier()` — mismatched nullifier rejected
- `test_withdraw_circuit_invalid_merkle_path()` — wrong path rejected
- `test_withdraw_circuit_zero_value()` — zero value rejected
- `test_withdraw_circuit_asset_mismatch()` — wrong asset_id rejected
- `test_withdraw_circuit_wrong_address()` — target address encoded correctly
- `test_withdraw_circuit_constraint_count()` — verify ~3,551 constraints

**File**: `crates/shielded/src/circuit_transfer.rs` (new tests, ~12)
- `test_transfer_circuit_satisfiable_1in_1out()` — minimal transfer works
- `test_transfer_circuit_satisfiable_2in_2out()` — typical transfer works
- `test_transfer_circuit_nullifier_derivation()` — each nullifier matches input
- `test_transfer_circuit_merkle_path_valid()` — each input has valid path
- `test_transfer_circuit_value_conservation()` — outputs <= inputs
- `test_transfer_circuit_value_creation_rejected()` — outputs > inputs rejected
- `test_transfer_circuit_range_check_nonzero()` — zero value note rejected
- `test_transfer_circuit_asset_id_consistency()` — all notes match public asset
- `test_transfer_circuit_spending_rights()` — correct spending key satisfies
- `test_transfer_circuit_wrong_spending_key()` — wrong key rejected
- `test_transfer_circuit_4in_4out_consolidation()` — large transfer works
- `test_transfer_circuit_constraint_count()` — verify ~7,800 constraints for 2in/2out

**File**: `crates/shielded/src/prover.rs` (RealProver tests, ~12)
- `test_real_prover_setup_succeeds()` — key generation completes
- `test_real_prover_deposit_prove_verify()` — round-trip for deposit circuit
- `test_real_prover_withdraw_prove_verify()` — round-trip for withdraw circuit
- `test_real_prover_transfer_prove_verify()` — round-trip for transfer circuit
- `test_real_prover_tampered_proof_rejected()` — flip a bit in proof data
- `test_real_prover_wrong_circuit_rejected()` — deposit proof fails withdraw verify
- `test_real_prover_public_input_tampered()` — modified nullifier rejected
- `test_real_prover_key_serialization()` — save/load keys round-trip
- `test_real_prover_proof_size()` — verify ~128B compressed proof
- `test_real_prover_multiple_proofs()` — generate many proofs with same keys
- `test_real_prover_deterministic_proof()` — same witness, different randomness
- `test_real_prover_performance()` — prove < 5s, verify < 10ms

**File**: `crates/shielded/src/proof_ser.rs` (new tests, ~6)
- `test_serialize_deserialize_roundtrip()` — valid proof survives round-trip
- `test_serialize_invalid_point()` — point at infinity rejected
- `test_deserialize_truncated_data()` — short buffer returns error
- `test_deserialize_garbage_data()` — random bytes returns error
- `test_serialize_size_compressed()` — compressed format is 128B
- `test_serialize_with_public_inputs()` — public inputs appended correctly

**File**: `crates/shielded/src/keygen.rs` (new tests, ~4)
- `test_keygen_generate_keys()` — produces non-empty PK and VK
- `test_keygen_save_load_roundtrip()` — keys survive disk round-trip
- `test_keygen_metadata()` — constraint count and input count correct
- `test_keygen_different_circuits()` — deposit and transfer get different keys

---

## [ ] Phase 12: Integration Tests — Not Implemented

**File**: `crates/protocol/tests/test_shielded_flow.rs` (extend existing, ~15 tests)
- `test_real_deposit_proof()` — generate real Groth16 proof for deposit
- `test_real_transfer_proof()` — 2-in/2-out transfer with real proof
- `test_real_withdraw_proof()` — withdraw to transparent address
- `test_real_proof_verify_valid()` — valid proof passes verification
- `test_real_proof_reject_tampered()` — tampered proof fails
- `test_real_proof_reject_double_spend()` — reuse nullifier rejected
- `test_real_deposit_then_transfer_flow()` — deposit → transfer → withdraw end-to-end
- `test_real_proving_time()` — measure proof generation time, assert < 5s
- `test_real_multi_asset_shielded()` — different asset IDs get different proofs
- `test_real_deposit_creates_commitment()` — commitment inserted in merkle tree
- `test_real_withdraw_consumes_note()` — nullifier marked spent after withdraw
- `test_real_transfer_preserves_value()` — value conservation end-to-end
- `test_real_compliance_kyc_shielded()` — KYC check on shielded deposit
- `test_real_compliance_whitelist_shielded()` — whitelist check on shielded transfer
- `test_real_note_encryption_roundtrip()` — encrypt note, decrypt with IVK

**File**: `crates/protocol/tests/integration/test_shielded_integration.rs` (new file, ~8 tests)
- `test_shielded_with_protocol_transaction()` — ShieldedDeposit instruction executes
- `test_shielded_with_protocol_transaction()` — ShieldedTransfer instruction executes
- `test_shielded_with_protocol_transaction()` — ShieldedWithdraw instruction executes
- `test_shielded_balance_deduct_on_deposit()` — transparent balance decreases
- `test_shielded_balance_credit_on_withdraw()` — transparent balance increases
- `test_shielded_nullifier_set_updated()` — nullifier tracked after transfer
- `test_shielded_merkle_tree_grows()` — commitments added to tree
- `test_shielded_gas_cost()` — shielded tx uses correct gas amount

---

## [ ] Phase 13: E2E Tests — Not Implemented

**File**: `crates/node/tests/test_shielded_e2e.rs` (new file, ~8 tests)
- `test_e2e_shielded_deposit_flow()` — node accepts deposit, commitment in tree
- `test_e2e_shielded_transfer_between_nodes()` — tx propagates, proof verifies on peer
- `test_e2e_shielded_withdraw_flow()` — node processes withdraw, credits transparent balance
- `test_e2e_shielded_double_spend_rejected()` — same nullifier submitted twice, second rejected
- `test_e2e_shielded_invalid_proof_rejected()` — tampered proof rejected by node
- `test_e2e_shielded_per_block_limit()` — 51st shielded tx in block rejected
- `test_e2e_shielded_multi_node_consensus()` — all nodes agree on shielded state after block
- `test_e2e_shielded_deposit_then_transfer_then_withdraw()` — full lifecycle across nodes

---

## Verification

```bash
# Build
cargo build -p call-shielded --features real-prover

# Unit tests (mock prover, fast)
cargo test -p call-shielded

# Real prover tests (slower, ~30s total)
cargo test -p call-shielded --features real-prover

# Integration tests
cargo test -p call-protocol --test test_shielded_flow --features real-prover

# Verify all existing tests still pass
cargo test -p call-shielded 2>&1 | tail -5
# Expected: 55 passing (existing) + new real prover tests
```

## Implementation Order

1. **Phase 1** (deps) — 0.5 day
2. **Phase 2** (Poseidon hash) — 2 days
3. **Phase 6** (Deposit circuit, simplest) — 1 day
4. **Phase 5** (Withdraw circuit, medium) — 3 days
5. **Phase 4** (Transfer circuit, most complex) — 5 days
6. **Phase 7** (RealProver + serialization) — 3 days
7. **Phase 3** (Poseidon Merkle tree) — 1 day (parallel with Phase 4)
8. **Phase 10** (Keygen) — 1 day
9. **Phase 8** (Wire into lib.rs + protocol) — 2 days
10. **Phase 9** (Integration tests) — 4 days

Total: ~22 days (matches zk.md estimate of ~23 days)

## Feature Flag Strategy

```toml
[features]
default = []
real-prover = []  # Enables arkworks + real Groth16 proving
```

- Default build: mock prover (fast CI, no heavy deps)
- `--features real-prover`: full arkworks stack, real proofs
- Existing 55 tests pass in both modes
- New real-prover tests only run with feature flag

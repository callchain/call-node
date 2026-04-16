# ZK Shielded Transaction: Real Prover Implementation

## Context

The `call-shielded` crate has a fully implemented ZK proving layer using arkworks + BN254 Groth16, with all 3 circuits (Deposit, Transfer, Withdraw), real prover verification wired into protocol instruction execution, and comprehensive test coverage.

## Test Summary

| Test Suite | Count | Status |
|------------|-------|--------|
| Shielded unit tests (mock) | 55 | Passing |
| Shielded unit tests (real-prover) | 122 | Passing |
| Protocol integration tests | 44 | Passing |
| E2E shielded tests | 7 | Passing |
| Real prover proof cycle tests | 15 | Passing |

---

## [x] Phase 1: arkworks Dependencies — Implemented

Added arkworks 0.4 dependencies to `crates/shielded/Cargo.toml` with `real-prover` feature flag.

---

## [x] Phase 2: Poseidon Hash Module — Implemented

`crates/shielded/src/poseidon.rs` — Poseidon hash for BN254 Fr field with plain Rust and circuit gadget modes, domain separation, 15 tests.

---

## [x] Phase 3: Poseidon Merkle Tree — Implemented

`crates/shielded/src/merkle_poseidon.rs` — Incremental Merkle tree using Poseidon hash, depth 32, 10 tests.

---

## [x] Phase 4: ShieldedTransfer Circuit — Implemented

`crates/shielded/src/circuit_transfer.rs` — R1CS ConstraintSynthesizer for BN254 with 5 constraints (nullifier derivation, merkle path, spending rights, value conservation, range/asset validity), 12 tests.

---

## [x] Phase 5: ShieldedWithdraw Circuit — Implemented

`crates/shielded/src/circuit_withdraw.rs` — Subset of Transfer with 4 constraints, 10 tests.

---

## [x] Phase 6: ShieldedDeposit Circuit — Implemented

`crates/shielded/src/circuit_deposit.rs` — Simplest circuit with 3 constraints, 8 tests.

---

## [x] Phase 7: RealProver Implementation — Implemented

`crates/shielded/src/prover.rs` — `RealProver` struct with ark-groth16 BN254, `setup()`, prove/verify methods for all 3 circuit types. `proof_ser.rs` for serialization (128B compressed proof). Singleton via `RealProver::global()`.

---

## [x] Phase 8: Wire into lib.rs and Protocol — Implemented

**`crates/shielded/src/lib.rs`**:
- `verify_shielded_proof(proof, circuit_type)` — delegates to `RealProver` when `real-prover` feature enabled, falls back to structural validation otherwise
- `ShieldedState` has `process_transfer`, `process_deposit`, `process_withdraw` methods
- `verify_zk_proof` allows empty commitments (withdraw) AND empty nullifiers (deposit)

**`crates/protocol/src/instructions.rs`**:
- `ShieldedTransfer`, `ShieldedWithdraw`, `ShieldedDeposit` instruction variants extended with `nullifiers`, `commitments`, `encrypted_notes` fields
- All 3 wired to call `ShieldedState` methods
- `ShieldedTransfer` and `ShieldedWithdraw` call `verify_shielded_proof` for real Groth16 verification when `real-prover` feature is enabled

**`crates/protocol/Cargo.toml`**:
- Added `real-prover` feature flag that passes through to `call-shielded/real-prover`

---

## [x] Phase 9: Integration Tests — Implemented

`crates/protocol/tests/test_shielded_flow.rs` — 15 real prover tests: deposit/transfer/withdraw proof cycles, tampered proof rejection, double-spend rejection, proving time benchmark, multi-asset, compliance, note encryption.

---

## [x] Phase 10: CRS Generation — Implemented

`crates/shielded/src/keygen.rs` — Dev keygen via `Groth16::circuit_specific_setup`, save/load keys to disk, key info metadata, 6 tests.

---

## [x] Phase 11: Unit Tests — Implemented

All unit tests passing:
- `poseidon.rs`: 15 tests
- `merkle_poseidon.rs`: 10 tests
- `circuit_deposit.rs`: 8 tests
- `circuit_withdraw.rs`: 10 tests
- `circuit_transfer.rs`: 12 tests
- `prover.rs` (RealProver): 6 tests + 7 prover tests
- `proof_ser.rs`: 9 tests
- `keygen.rs`: 6 tests
- Core modules: 16 tests

Total: 122 tests with real-prover, 55 without.

---

## [x] Phase 12: Integration Tests — Implemented

- `crates/protocol/tests/test_shielded_flow.rs` (extended, 15 tests)
- `crates/protocol/tests/test_shielded_integration.rs` (new, 13 tests)

Coverage: deposit/withdraw/transfer instruction execution, transparent balance deduction/credit, nullifier tracking, merkle tree growth, gas cost, compliance, per-block limits.

---

## [x] Phase 13: E2E Tests — Implemented

`crates/node/tests/test_shielded_e2e.rs` (7 tests):
- `test_e2e_shielded_deposit_flow`
- `test_e2e_shielded_withdraw_flow`
- `test_e2e_shielded_double_spend_rejected`
- `test_e2e_shielded_invalid_proof_rejected`
- `test_e2e_shielded_per_block_limit`
- `test_e2e_shielded_multi_node_consensus`
- `test_e2e_shielded_lifecycle`

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

# E2E tests
cargo test -p call-node --test test_shielded_e2e
```

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

# Stub / TODO / Placeholder Inventory

Generated from scanning `crates/` for `TODO`, `FIXME`, `stub`, `placeholder`, `PLACEHOLDER` markers.
Each entry is a task to be implemented and marked as done when complete.

---

## S1: Database Initialization Stub

**File**: `crates/storage/src/db.rs`

**Current state**: `open_db()` only creates directories, returns empty `CallDb` placeholder.
**TODO comment**: "replace with reth-db initialization"
**Task**: Integrate a real database backend. Since reth-db is deferred, implement a file-based RocksDB/sled backend or at minimum wire the existing in-memory state to disk serialization.
**Status**: [x] (implemented — file-based JSON persistence with CallDb, save/load PruneState, directory structure)

---

## S2: Pruning Functions (5 stubs)

**File**: `crates/storage/src/prune.rs` (lines 96-124)

**Current state**: All five functions accept `_` parameters and return `Ok(())`:
- `prune_execution_traces(_prune_boundary)` — protocol layer not wired
- `prune_receipts(_prune_boundary)` — receipt tables not wired
- `prune_block_bodies(_prune_boundary)` — block tables not wired
- `prune_old_snapshots(_keep)` — snapshot storage not wired
- `compact_database()` — DB backend not wired

**Task**: Implement in-memory pruning logic that tracks block heights and simulates the layered retention policy. Wire to disk serialization once S1 is done.
**Status**: [x] (implemented — in-memory tracking with PruneState struct and layered retention policy)

---

## S3: Fast Sync Flow (3 stubs)

**File**: `crates/storage/src/prune.rs` (lines 156-185)

**Current state**:
- `FastSyncFlow::download_and_verify()` — returns `NotFound`
- `FastSyncFlow::restore_snapshot()` — empty body
- `FastSyncFlow::incremental_sync()` — returns `Ok(0)`

**Task**: Implement snapshot-based fast sync: serialize `StateSnapshot` to disk, verify 2/3 quorum, restore state, incremental block catch-up.
**Status**: [x] (implemented — file-based snapshot serialization/deserialization, quorum verification, disk persistence)

---

## S4: EVM Execution in Block

**File**: `crates/consensus/src/block.rs` (lines 229, 273, 275)

**Current state**:
- Step 1 only counts EVM txs: `result.evm_tx_count = self.evm_txs.len();`
- `evm_state_root = Hash::ZERO` — placeholder
- `receipt_root = Hash::ZERO` — placeholder

**Task**: Wire `call-evm` executor into block execution: execute each `EvmTx`, collect gas used, compute EVM state trie root, generate receipts.
**Status**: [x] (implemented — EvmExecutor wired, state root computed from EvmState, receipt root from results)

---

## S5: Genesis Loading

**File**: `crates/node/src/boot.rs` (line 35)

**Current state**: Logs "genesis loaded (placeholder)" without actually loading genesis.
**Task**: Parse genesis file (JSON), validate hash, initialize balances/assets/validators from genesis data.
**Status**: [x] (implemented — `Genesis::load()`/`Genesis::apply()` with hex address parsing)

---

## S6: Telemetry Alert Rules

**File**: `crates/node/src/telemetry.rs` (lines 343-356)

**Current state**:
- `high_memory_usage` alert always returns `false`
- `low_disk_space` alert always returns `false`

**Task**: Implement real monitoring using OS-level APIs (sysinfo crate for memory, std::fs for disk space on data dir).
**Status**: [x] (implemented — sysinfo for RAM, fs2 for disk space, TelemetryRegistry with data_dir)

---

## S7: TX Size Estimation

**File**: `crates/payload-builder/src/builder.rs` (line 147, 303-308)

**Current state**: Rough estimate `120 + instructions.len() * 50` bytes.
**Task**: Implement actual serde-based size estimation using `serde_json::to_vec(tx).len()` or equivalent serialization measurement.
**Status**: [x] (implemented — `serde_json::to_vec(tx).map(|v| v.len())`)

---

## S8: EVM Gas Estimate in Payload Builder

**File**: `crates/payload-builder/src/builder.rs` (line 173-174)

**Current state**: `gas_estimate = evm_tx.len() as u64 * 10` — rough byte-based estimate.
**Task**: Parse EVM tx header to extract gas_limit field, use actual value for selection.
**Status**: [x] (implemented — `serde_json::from_slice::<EvmTransaction>(&evm_tx).map(|tx| tx.gas_limit)`)

---

## S9: Shielded Dev Circuit Stubs

**File**: `crates/shielded/src/prover.rs` (lines 288-302)

**Current state**: `dev_deposit_circuit()`, `dev_withdraw_circuit()`, `dev_transfer_circuit()` — these delegate to `setup_*_circuit()` helpers.
**Task**: These are intentional dev stubs for trusted setup. Already functional.
**Status**: [x] (already functional, no action needed)

---

## S10: EVM Executor PUSH32 Comment

**File**: `crates/evm/src/executor.rs` (line 373)

**Current state**: Comment says "placeholder" but code uses real `keccak256(name.as_bytes())`.
**Task**: Already functional, just remove misleading comment.
**Status**: [x] (already functional)

---

## S11: RPC Test Placeholders

**File**: `crates/rpc/src/tests.rs` (lines 220-234)

**Current state**: Tests that acknowledge incomplete features return empty results.
**Task**: Implement real log indexing and tx-by-reference lookup when receipt tables are wired.
**Status**: [ ] (deferred until S1/S4)

---

## S12: RPC Handler Placeholder Signature

**File**: `crates/rpc/src/handlers.rs` (line 453)

**Current state**: `signature: [0u8; 65]` in test/placeholder transaction construction.
**Task**: Accept optional signature parameter or use Ed25519 signing for real auth.
**Status**: [x] (implemented — optional signature parameter with keccak256 fallback)

---

## Implementation Priority

```
Completed: S1, S2, S3, S4, S5, S6, S7, S8, S9, S10, S12
Deferred: S11 (rpc tests) — depends on reth-db integration for receipt tables
```

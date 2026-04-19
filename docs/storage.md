# Callchain Storage Layer

## Overview

The Storage Layer (`crates/storage`) provides persistence for all Callchain state: protocol balances, asset registry, shielded pool, EVM state, consensus blocks, receipts, and more. It is designed around reth-db (MDBX) as the primary backend with a JSON file fallback for testing scenarios.

**Key design decisions:**
- reth-db (MDBX) for high-performance key-value storage
- 34 logical tables covering all subsystems
- Layered pruning strategy with configurable retention
- State snapshots for fast sync
- JSON fallback when MDBX is unavailable

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Storage Layer                                               │
│                                                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │   CallDb     │  │  PruneState  │  │  StateSnapshot   │  │
│  │  (reth-db    │  │  (in-memory  │  │  (Merkle roots   │  │
│  │   or JSON)   │  │   tracking)  │  │   + signatures)  │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  34 Tables (descriptors)                                ││
│  │  protocol_assets, protocol_balances, evm_accounts,      ││
│  │  shielded_merkle_tree, bridge_pending_ops,              ││
│  │  consensus_blocks, governance_proposals, ...            ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Database Initialization (`db.rs`)

`CallDb` wraps the reth-db `DatabaseEnv` (MDBX) with JSON fallback:

```rust
pub struct CallDb {
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub prune_dir: PathBuf,
    pub db: Option<Arc<DatabaseEnv>>,  // None = JSON fallback
}
```

**`open_db()`** attempts MDBX initialization; on failure, logs a warning and falls back to JSON file persistence.

**Gap #1 — reth-db integration is deferred:** The crate doc comment states "reth-db (MDBX) integration is deferred until the reth dependency supports our Rust toolchain." In practice, `init_call_db()` may succeed but the actual table read/write operations are not wired to MDBX. All state persistence goes through JSON files or is in-memory only.

**Gap #2 — JSON fallback is not production-grade:** When MDBX fails to initialize, the system silently falls back to JSON file persistence. JSON is not suitable for high-throughput blockchain state (no ACID, no concurrent writes, poor random access). Production nodes should refuse to start without a working MDBX instance.

### 2. Table Definitions (`tables.rs`)

34 table descriptors are defined with `name` and `description` fields:

| Category | Tables |
|----------|--------|
| Protocol | `protocol_assets`, `protocol_balances`, `protocol_allowances` |
| Shielded | `shielded_merkle_tree`, `shielded_nullifiers`, `shielded_commitments`, `shielded_viewing_keys` |
| Agent | `agent_registrations`, `agent_balances`, `agent_nonces` |
| EVM | `evm_accounts`, `evm_contracts`, `evm_storage` |
| Bridge | `bridge_pending_ops` |
| Consensus | `consensus_blocks`, `consensus_state` |
| Metadata | `metadata_chain_id`, `metadata_validators`, `metadata_compliance`, `metadata_agents` |
| Receipts | `receipts`, `logs`, `memos` |
| Fee/Oracle | `fee_currency_registry`, `oracle_prices`, `oracle_validator_info` |
| Governance | `governance_proposals`, `vote_delegations` |
| Sponsorship | `sponsor_auths`, `sponsor_pools`, `sponsor_daily_usage` |
| Security | `session_keys`, `multi_sig_configs`, `social_recovery_configs` |

**Gap #3 — Table descriptors are not actual database tables:** `all_tables()` returns an array of `TableDef { name, description }` structs. There is no MDBX table registration, no schema definition, no `Table` trait implementation, and no `DbCursor`/`DbTx` read/write logic. The tables exist as documentation only.

**Gap #4 — No data migration strategy:** With 34 logical tables and an evolving schema, there is no versioning or migration framework. Adding a new column or changing a data layout would require manual migration or full resync.

### 3. Pruning (`prune.rs`)

`PruneConfig` defines layered retention:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `snapshot_interval` | 100,000 blocks | Full state snapshot frequency |
| `snapshot_keep` | 3 | Recent snapshots to retain |
| `prune_interval` | 10,000 blocks | Prune check frequency |
| `keep_recent` | 50,000 | Full state retention |
| `keep_block_body` | 100,000 | Block body retention |
| `keep_receipt` | 1,000,000 | Receipt/log retention |

**PruneState** tracks execution traces, receipts, block bodies, and snapshots in-memory (BTreeMap + VecDeque).

**`maybe_prune()`** runs at intervals and removes entries beyond retention windows.

**Gap #5 — Pruning is in-memory only:** `PruneState` holds all data in `BTreeMap<u64, Vec<T>>`. There is no integration with the actual database. `maybe_prune()` trims in-memory structures but does not delete from disk. In a production node with MDBX, old data would continue to accumulate on disk.

**Gap #6 — `compact_database()` is a no-op:** The function sets `compaction_pending = true` and immediately clears it. No actual MDBX compaction is performed.

### 4. State Snapshots (`prune.rs`)

`StateSnapshot` contains Merkle roots of all state sub-tries at a given height, signed by 2/3 of validators:

```rust
pub struct StateSnapshot {
    pub height: u64,
    pub protocol_root: Hash,
    pub evm_root: Hash,
    pub shielded_root: Hash,
    pub agent_root: Hash,
    pub consensus_root: Hash,
    pub total_size: u64,
    pub validator_signatures: Vec<ValidatorSignature>,
}
```

**Gap #7 — `verify_snapshot()` only counts signatures:** It checks `signatures.len() >= quorum` but does not verify the cryptographic validity of any signature. A snapshot with 144 dummy `[0u8; 65]` signatures passes verification.

**Gap #8 — Snapshot production is not implemented:** `FastSyncFlow::save_snapshot()` writes JSON to disk, but nothing in the block production pipeline calls it. Snapshots must be produced manually or by an external process.

**Gap #9 — `incremental_sync()` is stubbed:** `FastSyncFlow::incremental_sync()` returns `Ok(0)` with a comment "In a real implementation, fetch blocks from peers." A node syncing from a snapshot cannot catch up to the chain head.

**Gap #10 — Fast sync uses `peers.len()` as validator count:** `download_and_verify()` passes `peers.len() as u32 + 1` as the total validator count to `verify_snapshot()`. This is incorrect — the validator set size comes from consensus state, not the number of sync peers.

### 5. Node Modes

```rust
pub enum NodeMode {
    Validator,   // Full state + recent 100K blocks
    Full,        // Current state + pruned history (default)
    Light,       // Block headers only
    Archive,     // All historical data
}
```

**Gap #11 — Node mode is not enforced:** `NodeMode` exists as a config field but no code checks it to determine what data to store or serve. A "Light" node would still attempt to store everything.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | Crate root, `StorageError` enum |
| `db.rs` | `CallDb`, MDBX init, JSON fallback |
| `tables.rs` | 34 table descriptors (names only) |
| `prune.rs` | `PruneConfig`, `PruneState`, `FastSyncFlow`, `StateSnapshot` |
| `reth_db.rs` | reth-db integration helpers (table registration stubs) |
| `expiration.rs` | Data expiration policies |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| MDBX integration | 🔴 Not ready | Deferred; JSON fallback is not production-grade |
| Table schema | 🔴 Not ready | 34 descriptors only, no actual read/write logic |
| Pruning | 🟡 Partial | In-memory tracking works, no DB integration |
| State snapshots | 🟡 Partial | Format defined, signature verification missing, production not wired |
| Fast sync | 🔴 Not ready | incremental_sync stubbed, snapshot production missing |
| Node modes | 🔴 Not ready | Enum exists but not enforced |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **reth-db integration deferred** | Critical | MDBX is the designed primary backend but integration is incomplete. Production nodes would run on JSON files. |
| 2 | **JSON fallback silently accepted** | High | When MDBX fails, the system falls back to JSON without failing hard. JSON cannot handle concurrent writes or high throughput. |
| 3 | **Table descriptors are documentation-only** | Critical | `all_tables()` returns name+description pairs. No actual MDBX table creation, no `DbCursor`, no read/write path. |
| 4 | **No database migration framework** | High | No schema versioning. Changes to data layout require manual migration or full resync. |
| 5 | **Pruning is in-memory only** | High | `maybe_prune()` trims BTreeMaps but never deletes from the actual database. Disk usage grows unbounded. |
| 6 | **`compact_database()` is a no-op** | Medium | Sets and immediately clears a flag. No MDBX compaction is performed. |
| 7 | **Snapshot signatures not verified** | Critical | `verify_snapshot()` counts signatures but does not cryptographically verify them. Fake snapshots pass. |
| 8 | **Snapshot production not wired** | High | No code path produces snapshots during normal operation. Must be done manually. |
| 9 | **`incremental_sync()` stubbed** | Critical | Returns `Ok(0)`. A node syncing from snapshot can never catch up. |
| 10 | **Fast sync uses peer count as validator count** | High | `verify_snapshot()` receives `peers.len() + 1` instead of actual validator set size. Quorum math is wrong. |
| 11 | **Node mode not enforced** | Medium | `NodeMode` config is ignored. Light/Archive distinctions are not implemented. |
| 12 | **No Write-Ahead Log (WAL)** | High | Without MDBX integration, there is no transaction log. Crashes can corrupt state. |
| 13 | **No backup/restore mechanism** | Medium | Beyond JSON file snapshots, there is no documented backup strategy for production operators. |
| 14 | **No storage metrics/telemetry** | Low | No disk usage, IOPS, or compaction metrics exposed. |

---

## Test Status

- `cargo test -p call-storage` — unit tests cover prune config defaults, node mode variants, snapshot verification (count-only), prune state tracking, fast sync error paths
- Missing: MDBX read/write tests, migration tests, compaction tests, concurrent access tests, corruption recovery tests

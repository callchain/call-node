# Callchain Storage Layer

## Overview

The Storage Layer (`crates/storage`) provides persistence for all Callchain state: protocol balances, asset registry, shielded pool, EVM state, consensus blocks, receipts, and more. It uses reth-db (MDBX) as its sole persistence backend — there is no JSON fallback.

**Key design decisions:**
- reth-db (MDBX) for high-performance key-value storage (sole backend, no fallback)
- 40 MDBX tables covering all subsystems (raw byte KV with serde_json serialization)
- Layered pruning strategy with configurable retention per node mode
- State snapshots for fast sync, produced automatically at interval boundaries
- Ed25519 signature verification with 2/3 quorum check for snapshot validity

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Storage Layer                                               │
│                                                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │   CallDb     │  │  PruneState  │  │  StateSnapshot   │  │
│  │  (MDBX only) │  │  (in-memory  │  │  (Merkle roots   │  │
│  │              │  │   tracking)  │  │   + signatures)  │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  40 MDBX Tables (CallTables: TableSet)                  ││
│  │  Raw byte KV with serde_json serialization              ││
│  │  CRUD helpers: db_put, db_get, db_del, db_iter_all,     ││
│  │  db_batch_put, db_clear, compact_db                     ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Database Initialization (`db.rs`)

`CallDb` wraps the reth-db `DatabaseEnv` (MDBX):

```rust
pub struct CallDb {
    pub data_dir: PathBuf,
    pub db: Arc<DatabaseEnv>,  // MDBX — always present after successful open
}
```

**`open_db()`** initializes MDBX. If initialization fails, it returns an error immediately — there is no fallback. The node refuses to start without a working MDBX instance.

### 2. Table Definitions (`reth_db.rs`)

40 MDBX tables are defined as structs implementing `reth_db_api::table::Table`:

| Category | Tables |
|----------|--------|
| EVM | `CallEvmAccounts`, `CallEvmStorage` |
| Trie | `CallTrieUpdates`, `CallAccountTrie`, `CallStorageTrie` |
| History | `CallAccountHistory`, `CallStorageHistory`, `CallBlockStateSnapshots` |
| Block Index | `CallBlockHashIndex`, `CallBlockHashByHeight` |
| Consensus | `CallConsensusBlocks`, `CallConsensusState` |
| Receipts | `CallReceipts`, `CallReceiptsByBlock` |
| Metadata | `CallMetadataChainId` |
| Light Client | `CallLightClientHeaders` |
| RPC | `CallRpcFilters` |
| Bytecode | `CallBytecodes` |
| System | `CallPruneState`, `CallFeeParams`, `CallForkState`, `CallCheckpoint` |

22 MDBX tables are registered via `CallTables: TableSet` and initialized with `init_db_for::<_, CallTables>`. All tables use `Vec<u8>` key/value with serde_json serialization. Protocol state (balances, assets, validators, etc.) lives in EVM storage slots (`CallEvmStorage`) under precompile addresses — no separate protocol-layer tables.

**CRUD helpers** in `reth_db.rs`:
- `db_put<T>`, `db_get<T>`, `db_del<T>` — single key operations
- `db_iter_all<T>` — full table scan
- `db_batch_put<T>` — bulk insert in single transaction
- `db_clear<T>` — truncate a table
- `compact_db` — MDBX flush/freelist compaction

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
| `node_mode` | `Full` | Node operating mode |

**`maybe_prune()`** enforces node mode behavior:

| Mode | Behavior |
|------|----------|
| `Archive` | No pruning — all historical data retained |
| `Light` | Aggressive — keep only recent 1,000 blocks |
| `Full` / `Validator` | Standard layered retention per config |

When `db: Option<&DatabaseEnv>` is provided, `maybe_prune()` deletes pruned entries from MDBX via `db_del` on the appropriate tables.

**`compact_database()`** calls `compact_db()` to commit a flush transaction to MDBX, ensuring freed pages are properly tracked on the freelist.

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

**`produce_state_snapshot()`** — called by the block production pipeline at `snapshot_interval` boundaries. The EVM state root is computed by reth-trie. `protocol_root` and `consensus_root` are set equal to `evm_root` since all state lives in EVM storage. `shielded_root` and `agent_root` are read from their respective precompile storage slots. The snapshot is recorded in `PruneState` and saved to `<data_dir>/snapshots/snapshot-{height}.json`.

**`verify_snapshot()`** — performs cryptographic Ed25519 verification of each validator signature against the snapshot message hash, then checks that at least 2/3 of the validator set signed. If no public keys are provided, falls back to count-only mode (tests only).

**Root computation:**
- `evm_root` — computed by reth-trie from `CallEvmAccounts` and `CallEvmStorage`
- `protocol_root` — equal to `evm_root` (all protocol state lives in EVM storage)
- `consensus_root` — equal to `evm_root` (validator state lives in EVM storage under `0x204`)
- `shielded_root` — read from EVM storage under `SHIELDED_ADDRESS` (`0x202`)
- `agent_root` — read from EVM storage under `AGENT_ADDRESS` (`0x209`)

### 5. Fast Sync (`prune.rs`)

`FastSyncFlow` provides the snapshot-based sync pipeline:
1. **download_and_verify** — load latest snapshot from disk, verify 2/3 validator signatures against actual validator set
2. **restore_snapshot** — confirm snapshot integrity
3. **incremental_sync** — fetch remaining blocks from snapshot height to current (handled by consensus layer, not storage)

`download_and_verify()` requires `validator_pubkeys` to be non-empty — quorum is calculated from the actual validator set size, not peer count.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | Crate root, `StorageError` enum |
| `db.rs` | `CallDb`, MDBX initialization, prune state persistence |
| `tables.rs` | 34 logical table descriptors (documentation) |
| `reth_db.rs` | 40 actual MDBX tables, CRUD helpers, compaction, state save/load |
| `prune.rs` | `PruneConfig`, `PruneState`, `StateSnapshot`, pruning, snapshot production/verification, fast sync |
| `expiration.rs` | Data expiration policies |

---

## Production Readiness

| Component | Status | Notes |
|-----------|--------|-------|
| MDBX integration | ✅ Ready | 40 tables, full CRUD, no JSON fallback |
| Table schema | ✅ Ready | `CallTables: TableSet` with raw byte KV |
| Pruning | ✅ Ready | In-memory tracking + MDBX deletion, node mode enforcement, telemetry exposed |
| State snapshots | ✅ Ready | Production wired into block pipeline, Ed25519 verification |
| Fast sync | 🟡 Partial | `incremental_sync` returns 0 (handled by consensus layer) |
| Node modes | ✅ Ready | Archive/Light/Full/Validator enforced in `maybe_prune()` |

## Remaining Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 4 | **No database migration framework** | High | No schema versioning. Changes to data layout require manual migration or full resync. |
| 9 | **`incremental_sync()` returns 0** | Low | By design — storage has no network access. Post-snapshot catch-up is handled by the consensus block production loop receiving blocks from P2P peers. |

---

## Test Status

- `cargo test -p call-storage` — 34 tests covering prune config defaults, node mode variants (Archive/Light/Full/Validator), snapshot verification (count-only and Ed25519), prune state tracking, fast sync error paths (no peers, missing validator pubkeys), root computation determinism, snapshot production to disk, compaction flag behavior
- `cargo test -p call-node --lib` — 56 tests including MDBX prune state persistence, block production, and end-to-end node flows
- Missing: schema migration tests, concurrent access tests, corruption recovery tests, full Ed25519 snapshot verification with real keys

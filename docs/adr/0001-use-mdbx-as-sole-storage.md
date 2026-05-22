# ADR-0001: Use MDBX (reth-db) as Sole Persistence Backend

- Status: Accepted
- Date: 2026-04-13
- Author(s): Callchain Core Team

## Context

Callchain needs a single, reliable persistence layer for:

- EVM world state (accounts, storage slots, bytecode)
- Consensus blocks and headers
- Protocol receipts and receipt indexes
- Consensus state (validator sets, proposer subsets)
- Historical state for RPC queries (`eth_getBalance(blockTag)`, `eth_getStorageAt`, `eth_getProof`)
- Trie nodes for incremental state-root computation and proof generation
- Light-client verified headers
- RPC filter state

Early designs considered a hybrid approach: MDBX for EVM state plus flat files for blocks and metadata. This would have required two recovery paths, two backup strategies, and two consistency models. The team needed a unified backend that supports ACID transactions, crash recovery, concurrent readers, and incremental state-root computation out of the box.

## Decision

Use **MDBX via reth-db** as the sole persistence backend for all node state. All data is stored in custom tables defined in `crates/storage/src/reth_db.rs`, initialized through `reth_db::mdbx::init_db_for::<_, CallTables>`. The database lives at `<data_dir>/mdbx`.

Key tables include:

| Table | Purpose |
|-------|---------|
| `CallEvmAccounts` | Serialized EVM accounts keyed by address |
| `CallEvmStorage` | Storage slots keyed by `(address, slot)` |
| `CallConsensusBlocks` | Blocks keyed by height |
| `CallConsensusState` | Persisted consensus state snapshot |
| `CallReceipts` / `CallReceiptsByBlock` | Transaction receipts and block-level index |
| `CallAccountHistory` / `CallStorageHistory` | Historical state for RPC block-tag queries |
| `CallAccountTrie` / `CallStorageTrie` | Merkle-Patricia trie branch nodes for `eth_getProof` |
| `CallTrieUpdates` | Incremental trie updates per block for fast state-root recomputation |
| `CallBlockStateSnapshots` | Full EVM state snapshots pruned after 128 blocks |
| `CallBlockHashIndex` / `CallBlockHashByHeight` | O(1) block hash lookups |
| `CallBytecodes` | Bytecode keyed by keccak256 hash |
| `CallCheckpoint` | Crash-recovery checkpoint marker |
| `CallSchemaVersion` | Database schema version for migrations |

Serialization uses `serde_json` for all values to avoid the complexity of implementing `reth-codecs` traits for every custom type. Keys are raw byte vectors with stable layouts (e.g., `[address: 20][block_number: 8 BE]` for history tables).

## Consequences

### Positive

- **Single source of truth**: All state — EVM, consensus, protocol, RPC — lives in one ACID database. There is no risk of skew between a file-based block log and an MDBX state root.
- **Crash recovery**: MDBX's write-ahead log (WAL) replays committed transactions on unclean shutdown. The `CallCheckpoint` table provides an application-level marker for batch-completion detection.
- **Concurrent access**: MDBX supports multiple readers without locking. The integration tests in `reth_db.rs` verify concurrent writes to different keys and race-condition safety for same-key writes.
- **Incremental state root**: `CallTrieUpdates` stores the per-block delta from `reth-trie::StateRoot::root_with_updates()`, enabling O(delta) state-root computation instead of rebuilding the full trie.
- **RPC compatibility**: Historical tables and trie-node tables enable `eth_getBalance(blockTag)`, `eth_getStorageAt(blockTag)`, and `eth_getProof` without replaying blocks or recomputing tries from scratch.
- **Pruning support**: Snapshot and history tables are pruned after a configurable retention period (default 128 blocks), controlling disk growth.

### Negative / Trade-offs

- **serde_json overhead**: Using JSON instead of compact binary codecs increases value size and (de)serialization cost. This was accepted to avoid the engineering burden of maintaining `reth-codecs` implementations for ~20 custom types.
- **No native range delete**: MDBX does not support efficient prefix/range deletion. Clearing an account's storage on `SELFDESTRUCT` currently requires iterating all slots for that address, which is noted as a production TODO in `crates/evm/src/db.rs`.
- **Compaction requires restart**: While `compact_db()` commits an empty transaction to sync the freelist, full disk-space reclamation requires a node restart so MDBX can perform startup cleanup.
- **Schema migration burden**: Schema changes must be handled via the `CallSchemaVersion` table and incremental migration code on startup. There is no automatic schema-evolution framework.
- **Test `vm.max_map_count`**: Concurrent test databases can exhaust `vm.max_map_count`. The codebase mitigates this with `init_call_db_test()`, which uses a 64 MB max geometry for test databases.

## Alternatives Considered

- **Flat files for blocks + MDBX for state**: Rejected because it introduces dual consistency, dual backup, and dual recovery paths. A single database simplifies operations and eliminates a class of bugs.
- **RocksDB**: Rejected because reth-db already wraps MDBX, and MDBX offers better read performance, stricter ACID guarantees, and a smaller C footprint than RocksDB.
- **Custom LSM-tree**: Rejected as unnecessary engineering. MDBX is battle-tested in Reth and provides the exact features needed (WAL, snapshots, cursor API).

## References

- `crates/storage/src/reth_db.rs` — table definitions, init helpers, CRUD functions
- `crates/evm/src/db.rs` — `EvmDb` implementing `revm::DatabaseRef`, `apply_revm_state_to_mdbx`, trie update persistence
- `crates/evm/src/provider.rs` — `InMemoryStateProvider` save/load to MDBX
- `docs/spec.md` §2.4, §4 — block structure and EVM execution spec

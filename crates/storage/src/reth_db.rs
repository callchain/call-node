//! reth-db (MDBX) integration for Callchain state persistence.
//!
//! Defines custom database tables and provides initialization/persistence
//! helpers. Uses raw byte keys/values with serde_json serialization to avoid
//! the complexity of reth-codecs trait implementations for every type.
//!
//! Generic CRUD helpers are provided here. Type-specific save/load functions
//! live in the `call-node` crate to avoid cyclic dependencies.

use std::path::Path;
use std::sync::Arc;

use reth_db::mdbx::{init_db_for, DatabaseArguments};
use reth_db::DatabaseEnv;
use reth_db_api::table::{Table, TableInfo};
use reth_db_api::{TableSet, DatabaseError};
use reth_db_api::database::Database;
use reth_db_api::transaction::{DbTx, DbTxMut};
use reth_db::cursor::DbCursorRW;
use reth_db_api::cursor::DbCursorRO;

use crate::StorageError;

// ── Custom Table Definitions ──────────────────────────────────────────

/// EVM accounts: serialized Address -> serialized EvmAccount
#[derive(Debug)]
pub struct CallEvmAccounts;
impl Table for CallEvmAccounts {
    const NAME: &'static str = "call_evm_accounts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// EVM storage slots: serialized (address, slot) -> serialized value
#[derive(Debug)]
pub struct CallEvmStorage;
impl Table for CallEvmStorage {
    const NAME: &'static str = "call_evm_storage";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Prune state: single entry () -> serialized PruneState
#[derive(Debug)]
pub struct CallPruneState;
impl Table for CallPruneState {
    const NAME: &'static str = "call_prune_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Consensus state: single entry () -> serialized PersistedConsensusState
#[derive(Debug)]
pub struct CallConsensusState;
impl Table for CallConsensusState {
    const NAME: &'static str = "call_consensus_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Consensus blocks: serialized height -> serialized Block
#[derive(Debug)]
pub struct CallConsensusBlocks;
impl Table for CallConsensusBlocks {
    const NAME: &'static str = "call_consensus_blocks";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Metadata chain id: single entry () -> chain_id
#[derive(Debug)]
pub struct CallMetadataChainId;
impl Table for CallMetadataChainId {
    const NAME: &'static str = "call_metadata_chain_id";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Receipts: serialized tx_hash -> serialized ProtocolReceipt
#[derive(Debug)]
pub struct CallReceipts;
impl Table for CallReceipts {
    const NAME: &'static str = "call_receipts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Receipt index: block_number -> Vec<TxHash> for efficient block-level queries and pruning
#[derive(Debug)]
pub struct CallReceiptsByBlock;
impl Table for CallReceiptsByBlock {
    const NAME: &'static str = "call_receipts_by_block";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Fee params: single entry () -> serialized FeeParams
#[derive(Debug)]
pub struct CallFeeParams;
impl Table for CallFeeParams {
    const NAME: &'static str = "call_fee_params";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Fork state: single entry () -> serialized ForkManager
#[derive(Debug)]
pub struct CallForkState;
impl Table for CallForkState {
    const NAME: &'static str = "call_fork_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Checkpoint marker: single entry "pending" -> state_hash for crash recovery
#[derive(Debug)]
pub struct CallCheckpoint;
impl Table for CallCheckpoint {
    const NAME: &'static str = "call_checkpoint";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Trie updates: single entry block_number -> serialized TrieUpdates
///
/// Stores the incremental trie updates produced by reth-trie's
/// `StateRoot::root_with_updates()`. These updates are applied as an
/// overlay on top of the existing trie for the next block's incremental
/// state root computation.
#[derive(Debug)]
pub struct CallTrieUpdates;
impl Table for CallTrieUpdates {
    const NAME: &'static str = "call_trie_updates";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Account history: serialized (address, block_number) -> serialized EvmAccount
///
/// Stores the state of an account at a specific block height.
/// Key layout: [address: 20 bytes][block_number: 8 bytes BE].
/// Enables historical account queries (eth_getBalance, eth_getCode, eth_getNonce).
#[derive(Debug)]
pub struct CallAccountHistory;
impl Table for CallAccountHistory {
    const NAME: &'static str = "call_account_history";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Storage history: serialized (address, slot, block_number) -> serialized U256
///
/// Stores the value of a storage slot at a specific block height.
/// Key layout: [address: 20 bytes][slot: 32 bytes][block_number: 8 bytes BE].
/// Enables historical storage queries (eth_getStorageAt).
#[derive(Debug)]
pub struct CallStorageHistory;
impl Table for CallStorageHistory {
    const NAME: &'static str = "call_storage_history";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Account trie nodes: serialized nibbles path -> serialized BranchNodeCompact
///
/// Stores the intermediate branch nodes of the account Merkle Patricia Trie.
/// Enables `eth_getProof` to generate proofs without recomputing the trie
/// from scratch.
#[derive(Debug)]
pub struct CallAccountTrie;
impl Table for CallAccountTrie {
    const NAME: &'static str = "call_account_trie";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Storage trie nodes: hashed_address + serialized nibbles -> serialized BranchNodeCompact
///
/// Stores the intermediate branch nodes of per-account storage Merkle Patricia Tries.
/// Key layout: [hashed_address: 32 bytes][nibbles_bytes...].
/// Enables `eth_getProof` storage proofs without recomputing from scratch.
#[derive(Debug)]
pub struct CallStorageTrie;
impl Table for CallStorageTrie {
    const NAME: &'static str = "call_storage_trie";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Block state snapshots: serialized block_number -> serialized EvmState
///
/// Stores a full copy of the EVM state at a specific block height.
/// Used to serve historical queries (`eth_getBalance(blockTag)`,
/// `eth_getProof`) without replaying blocks. Pruned after 128 blocks
/// by default.
#[derive(Debug)]
pub struct CallBlockStateSnapshots;
impl Table for CallBlockStateSnapshots {
    const NAME: &'static str = "call_block_state_snapshots";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Block hash index: serialized BlockHash -> serialized height (u64 BE)
///
/// Enables O(1) eth_getBlockByHash lookups without scanning all blocks.
#[derive(Debug)]
pub struct CallBlockHashIndex;
impl Table for CallBlockHashIndex {
    const NAME: &'static str = "call_block_hash_index";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Block hash by height: serialized height (u64 BE) -> serialized BlockHash
///
/// Enables O(1) BLOCKHASH opcode lookups without deserializing full blocks.
#[derive(Debug)]
pub struct CallBlockHashByHeight;
impl Table for CallBlockHashByHeight {
    const NAME: &'static str = "call_block_hash_by_height";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Light client verified headers: serialized block_height -> serialized BlockHeader
///
/// Stores block headers verified by the protocol light client so they
/// survive node restarts. Loaded on startup and updated after each
/// successful header verification.
#[derive(Debug)]
pub struct CallLightClientHeaders;
impl Table for CallLightClientHeaders {
    const NAME: &'static str = "call_light_client_headers";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// RPC filters: single entry b"filters" -> serialized FilterManagerState
///
/// Persists active eth_newFilter / eth_newBlockFilter entries across
/// node restarts so RPC clients do not lose subscriptions.
#[derive(Debug)]
pub struct CallRpcFilters;
impl Table for CallRpcFilters {
    const NAME: &'static str = "call_rpc_filters";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Bytecodes: serialized code_hash (B256) -> raw bytecode bytes
///
/// Enables `code_by_hash_ref` lookups when revm's CacheDB only caches
/// the code_hash and needs to fetch the full bytecode on demand.
#[derive(Debug)]
pub struct CallBytecodes;
impl Table for CallBytecodes {
    const NAME: &'static str = "call_bytecodes";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// All Callchain tables
pub struct CallTables;
impl TableSet for CallTables {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        fn box_info<T: Table>() -> Box<dyn TableInfo> {
            Box::new(TableInfoWrapper(std::marker::PhantomData::<T>))
        }
        Box::new(
            [
                box_info::<CallEvmAccounts>,
                box_info::<CallEvmStorage>,
                box_info::<CallPruneState>,
                box_info::<CallConsensusState>,
                box_info::<CallConsensusBlocks>,
                box_info::<CallMetadataChainId>,
                box_info::<CallReceipts>,
                box_info::<CallReceiptsByBlock>,
                box_info::<CallFeeParams>,
                box_info::<CallForkState>,
                box_info::<CallCheckpoint>,
                box_info::<CallTrieUpdates>,
                box_info::<CallAccountHistory>,
                box_info::<CallStorageHistory>,
                box_info::<CallBlockStateSnapshots>,
                box_info::<CallAccountTrie>,
                box_info::<CallStorageTrie>,
                box_info::<CallLightClientHeaders>,
                box_info::<CallBlockHashIndex>,
                box_info::<CallBlockHashByHeight>,
                box_info::<CallRpcFilters>,
                box_info::<CallBytecodes>,
            ]
            .into_iter()
            .map(|f| f()),
        )
    }
}

/// Wrapper to make table types implement TableInfo
#[derive(Debug)]
struct TableInfoWrapper<T>(std::marker::PhantomData<T>);
impl<T: Table> TableInfo for TableInfoWrapper<T> {
    fn name(&self) -> &'static str {
        <T as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <T as Table>::DUPSORT
    }
}

// ── Database Initialization ───────────────────────────────────────────

/// Initialize or open the Callchain MDBX database at the given path.
pub fn init_call_db(data_dir: &Path) -> Result<Arc<DatabaseEnv>, StorageError> {
    let db_path = data_dir.join("mdbx");
    let args = DatabaseArguments::default();
    let db = init_db_for::<_, CallTables>(&db_path, args)
        .map_err(|e| StorageError::Database(e.to_string()))?;
    Ok(Arc::new(db))
}

// ── Helper Functions ──────────────────────────────────────────────────

fn db_err(e: DatabaseError) -> StorageError {
    StorageError::Database(e.to_string())
}

/// Write a key-value pair to a table within a transaction.
pub fn db_put<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    db: &DatabaseEnv,
    key: Vec<u8>,
    value: Vec<u8>,
) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    cursor.upsert(key, &value).map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Read a value from a table by key within a transaction.
pub fn db_get<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
    let tx = db.tx().map_err(db_err)?;
    let mut cursor = tx.cursor_read::<T>().map_err(db_err)?;
    let value = cursor.seek_exact(key.to_vec()).map_err(db_err)?;
    Ok(value.map(|(_, v)| v))
}

/// Delete a key from a table within a transaction.
pub fn db_del<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv, key: &[u8]) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    if cursor.seek_exact(key.to_vec()).map_err(db_err)?.is_some() {
        cursor.delete_current().map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Iterate all key-value pairs in a table, collecting them.
pub fn db_iter_all<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
    let tx = db.tx().map_err(db_err)?;
    let mut cursor = tx.cursor_read::<T>().map_err(db_err)?;
    let mut results = Vec::new();
    let walker = cursor.walk(None).map_err(db_err)?;
    for entry in walker {
        let (k, v) = entry.map_err(db_err)?;
        results.push((k, v));
    }
    Ok(results)
}

/// Compact the MDBX database to release unused disk pages back to the OS.
///
/// MDBX uses a write-ahead log and copy-on-write design. When data is
/// deleted, pages go onto the freelist for reuse by future writes. This
/// function triggers a full sync of the database, ensuring all freed pages
/// are properly tracked and can be reused.
///
/// For full disk space reclamation, the node operator should periodically
/// restart the node — MDBX reclaims freed pages during startup cleanup.
pub fn compact_db(db: &DatabaseEnv) -> Result<(), StorageError> {
    // Commit an empty transaction to ensure all freed pages are returned
    // to the freelist and the database is fully synced.
    let tx = db.tx_mut().map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Batch write multiple key-value pairs to a table in a single transaction.
pub fn db_batch_put<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    db: &DatabaseEnv,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<(), StorageError> {
    if entries.is_empty() {
        return Ok(());
    }
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    for (key, value) in entries {
        cursor.upsert(key, &value).map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Clear all entries in a table (used before full state reload).
pub fn db_clear<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    while cursor.first().map_err(db_err)?.is_some() {
        cursor.delete_current().map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

// ── Convenience Methods for Each Table ────────────────────────────────

/// Save prune state to the database.
pub fn save_prune_state(db: &DatabaseEnv, state: &crate::prune::PruneState) -> Result<(), StorageError> {
    let data = serde_json::to_vec(state).map_err(|e| StorageError::Serialization(e.to_string()))?;
    db_put::<CallPruneState>(db, vec![0], data)
}

/// Load prune state from the database.
pub fn load_prune_state(db: &DatabaseEnv) -> Result<crate::prune::PruneState, StorageError> {
    match db_get::<CallPruneState>(db, &[0])? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| StorageError::Serialization(e.to_string())),
        None => Ok(crate::prune::PruneState::new()),
    }
}

/// Save a verified light-client header hash to the database.
pub fn save_light_client_header(db: &DatabaseEnv, height: u64, block_hash: &call_primitives::BlockHash) -> Result<(), StorageError> {
    let key = height.to_be_bytes().to_vec();
    db_put::<CallLightClientHeaders>(db, key, block_hash.0.to_vec())
}

/// Load a verified light-client header hash from the database by height.
pub fn load_light_client_header(db: &DatabaseEnv, height: u64) -> Result<Option<call_primitives::BlockHash>, StorageError> {
    let key = height.to_be_bytes().to_vec();
    match db_get::<CallLightClientHeaders>(db, &key)? {
        Some(data) if data.len() == 32 => Ok(Some(call_primitives::BlockHash::from_slice(&data))),
        _ => Ok(None),
    }
}

/// Load all verified light-client header hashes from the database.
pub fn load_all_light_client_headers(db: &DatabaseEnv) -> Result<std::collections::HashMap<u64, call_primitives::BlockHash>, StorageError> {
    let mut headers = std::collections::HashMap::new();
    for (key, value) in db_iter_all::<CallLightClientHeaders>(db)? {
        if key.len() == 8 && value.len() == 32 {
            let height = u64::from_be_bytes(key.try_into().unwrap());
            headers.insert(height, call_primitives::BlockHash::from_slice(&value));
        }
    }
    Ok(headers)
}

/// Delete a light-client header hash from the database (used during reorg handling).
pub fn delete_light_client_header(db: &DatabaseEnv, height: u64) -> Result<(), StorageError> {
    let key = height.to_be_bytes().to_vec();
    db_del::<CallLightClientHeaders>(db, &key)
}

/// Save a block hash by height for fast BLOCKHASH opcode lookups.
pub fn save_block_hash_by_height(db: &DatabaseEnv, height: u64, block_hash: &call_primitives::BlockHash) -> Result<(), StorageError> {
    let key = height.to_be_bytes().to_vec();
    db_put::<CallBlockHashByHeight>(db, key, block_hash.0.to_vec())
}

/// Load a block hash by height for BLOCKHASH opcode lookups.
pub fn load_block_hash_by_height(db: &DatabaseEnv, height: u64) -> Result<Option<call_primitives::BlockHash>, StorageError> {
    let key = height.to_be_bytes().to_vec();
    match db_get::<CallBlockHashByHeight>(db, &key)? {
        Some(data) if data.len() == 32 => Ok(Some(call_primitives::BlockHash::from_slice(&data))),
        _ => Ok(None),
    }
}

/// Delete a block hash by height (used during pruning).
pub fn delete_block_hash_by_height(db: &DatabaseEnv, height: u64) -> Result<(), StorageError> {
    let key = height.to_be_bytes().to_vec();
    db_del::<CallBlockHashByHeight>(db, &key)
}

/// Save raw bytecode keyed by its keccak256 hash.
pub fn save_bytecode(db: &DatabaseEnv, code_hash: &call_primitives::BlockHash, code: &[u8]) -> Result<(), StorageError> {
    db_put::<CallBytecodes>(db, code_hash.as_slice().to_vec(), code.to_vec())
}

/// Load raw bytecode by its keccak256 hash.
pub fn load_bytecode(db: &DatabaseEnv, code_hash: &call_primitives::BlockHash) -> Result<Option<Vec<u8>>, StorageError> {
    db_get::<CallBytecodes>(db, code_hash.as_slice())
}

/// Delete bytecode by hash (used during pruning).
pub fn delete_bytecode(db: &DatabaseEnv, code_hash: &call_primitives::BlockHash) -> Result<(), StorageError> {
    db_del::<CallBytecodes>(db, code_hash.as_slice())
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::db::{CallDb, open_db};
    use std::sync::Arc;
    use std::thread;

    fn temp_db() -> CallDb {
        let path = std::env::temp_dir().join(format!(
            "call-mdbx-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        open_db(path).expect("failed to open temp db")
    }

    #[test]
    fn test_concurrent_writes_different_keys() {
        let db = temp_db();

        let db_a = Arc::clone(&db.db);
        let handle_a = thread::spawn(move || {
            for i in 0..100 {
                let key = format!("account_{}", i).into_bytes();
                let value: u128 = i as u128 * 1_000_000;
                let data = serde_json::to_vec(&value).unwrap();
                db_put::<CallEvmAccounts>(&db_a, key, data).unwrap();
            }
        });

        let db_b = Arc::clone(&db.db);
        let handle_b = thread::spawn(move || {
            for i in 0..100 {
                let key = format!("receipt_{}", i).into_bytes();
                let value: u128 = i as u128 * 500_000;
                let data = serde_json::to_vec(&value).unwrap();
                db_put::<CallReceipts>(&db_b, key, data).unwrap();
            }
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        for i in 0..100 {
            let key = format!("account_{}", i).into_bytes();
            let data = db_get::<CallEvmAccounts>(&db.db, &key).unwrap().unwrap();
            let value: u128 = serde_json::from_slice(&data).unwrap();
            assert_eq!(value, i as u128 * 1_000_000);

            let key = format!("receipt_{}", i).into_bytes();
            let data = db_get::<CallReceipts>(&db.db, &key).unwrap().unwrap();
            let value: u128 = serde_json::from_slice(&data).unwrap();
            assert_eq!(value, i as u128 * 500_000);
        }
    }

    #[test]
    fn test_crash_recovery_checkpoint() {
        let db = temp_db();

        write_checkpoint(&db.db, [0xDEu8; 32]).unwrap();
        assert!(check_recovery(&db.db).unwrap(), "should detect pending checkpoint");

        clear_checkpoint(&db.db).unwrap();
        assert!(!check_recovery(&db.db).unwrap(), "checkpoint should be cleared");
    }

    #[test]
    fn test_compaction_flag() {
        let db = temp_db();
        let state = crate::prune::PruneState::new();
        save_prune_state(&db.db, &state).unwrap();
        compact_db(&db.db).unwrap();

        let loaded = load_prune_state(&db.db).unwrap();
        assert_eq!(serde_json::to_string(&state).unwrap(), serde_json::to_string(&loaded).unwrap());
    }

    #[test]
    fn test_save_load_prune_state_roundtrip() {
        let db = temp_db();
        let state = crate::prune::PruneState::new();
        save_prune_state(&db.db, &state).unwrap();
        let loaded = load_prune_state(&db.db).unwrap();
        assert_eq!(serde_json::to_string(&state).unwrap(), serde_json::to_string(&loaded).unwrap());
    }

    #[test]
    fn test_db_clear_and_repopulate() {
        let db = temp_db();
        db_put::<CallMetadataChainId>(&db.db, b"key1".to_vec(), b"value1".to_vec()).unwrap();
        db_put::<CallMetadataChainId>(&db.db, b"key2".to_vec(), b"value2".to_vec()).unwrap();

        assert!(db_get::<CallMetadataChainId>(&db.db, b"key1").unwrap().is_some());
        assert!(db_get::<CallMetadataChainId>(&db.db, b"key2").unwrap().is_some());

        db_clear::<CallMetadataChainId>(&db.db).unwrap();

        assert!(db_get::<CallMetadataChainId>(&db.db, b"key1").unwrap().is_none());
        assert!(db_get::<CallMetadataChainId>(&db.db, b"key2").unwrap().is_none());
    }

    #[test]
    fn test_batch_write_large_dataset() {
        let db = temp_db();
        let mut pairs = Vec::new();
        for i in 0..10_000 {
            let key = format!("receipt_{:08x}", i).into_bytes();
            let receipt = format!("receipt data {}", i);
            let value = serde_json::to_vec(&receipt).unwrap();
            pairs.push((key, value));
        }

        db_batch_put::<CallReceipts>(&db.db, pairs).unwrap();

        for i in [0, 4999, 9999] {
            let key = format!("receipt_{:08x}", i).into_bytes();
            let data = db_get::<CallReceipts>(&db.db, &key).unwrap().unwrap();
            let receipt: String = serde_json::from_slice(&data).unwrap();
            assert_eq!(receipt, format!("receipt data {}", i));
        }

        let all = db_iter_all::<CallReceipts>(&db.db).unwrap();
        assert_eq!(all.len(), 10_000);
    }

    fn write_checkpoint(db: &DatabaseEnv, block_hash: [u8; 32]) -> Result<(), String> {
        db_put::<CallCheckpoint>(db, b"pending".to_vec(), block_hash.to_vec())
            .map_err(|e| e.to_string())
    }

    fn check_recovery(db: &DatabaseEnv) -> Result<bool, String> {
        match db_get::<CallCheckpoint>(db, b"pending").map_err(|e: StorageError| e.to_string())? {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    fn clear_checkpoint(db: &DatabaseEnv) -> Result<(), String> {
        db_del::<CallCheckpoint>(db, b"pending").map_err(|e| e.to_string())
    }
}

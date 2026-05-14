//! Persistent EVM state backed by MDBX (reth-db).
//!
//! Provides `EvmDb` implementing revm's `DatabaseRef` trait for direct
//! disk-backed execution, plus `InMemoryStateProvider` save/load helpers.

use crate::state::EvmAccount;
use alloy_primitives::{Address, B256, U256};
use call_storage::reth_db::{
    db_del, db_get, db_iter_all, db_put, CallAccountHistory, CallAccountTrie,
    CallBlockHashByHeight, CallBlockStateSnapshots, CallBytecodes, CallEvmAccounts, CallEvmStorage,
    CallStorageHistory, CallStorageTrie, CallTrieUpdates,
};
use reth_db::cursor::{DbCursorRO, DbCursorRW};
use reth_db::DatabaseEnv;
use reth_db_api::database::Database;
use reth_db_api::table::Table;
use reth_db_api::transaction::{DbTx, DbTxMut};
use revm::database_interface::{DatabaseRef, ErasedError};
use revm::{bytecode::Bytecode, state::AccountInfo};
use std::sync::Arc;

/// EVM database backed by MDBX.
///
/// Implements revm's `DatabaseRef` so revm can read accounts, storage,
/// and code directly from persistent storage during execution.
#[derive(Debug, Clone)]
pub struct EvmDb {
    db: Arc<DatabaseEnv>,
}

impl EvmDb {
    /// Open the EVM database backed by the given MDBX environment.
    pub fn new(db: Arc<DatabaseEnv>) -> Self {
        Self { db }
    }

    /// Load the full `InMemoryStateProvider` from MDBX.
    pub fn load_state(&self) -> Result<crate::provider::InMemoryStateProvider, ErasedError> {
        crate::provider::InMemoryStateProvider::from_db(&self.db).map_err(|e| ErasedError::new(e))
    }

    /// Save the full `InMemoryStateProvider` to MDBX.
    pub fn save_state(
        &self,
        state: &crate::provider::InMemoryStateProvider,
    ) -> Result<(), ErasedError> {
        state.save_to_db(&self.db)
    }
}

impl DatabaseRef for EvmDb {
    type Error = ErasedError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let key = address.as_slice().to_vec();
        match db_get::<CallEvmAccounts>(&self.db, &key).map_err(ErasedError::new)? {
            Some(bytes) => {
                let account: EvmAccount =
                    serde_json::from_slice(&bytes).map_err(ErasedError::new)?;
                let code = if account.code.is_empty() {
                    None
                } else {
                    Some(Bytecode::new_raw(account.code))
                };
                Ok(Some(AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: code
                        .as_ref()
                        .map(|c| c.hash_slow())
                        .unwrap_or(revm::primitives::KECCAK_EMPTY),
                    code,
                    account_id: None,
                }))
            }
            None => Ok(None),
        }
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        let mut key = Vec::with_capacity(52);
        key.extend_from_slice(address.as_slice());
        key.extend_from_slice(&index.to_be_bytes::<32>());
        match db_get::<CallEvmStorage>(&self.db, &key).map_err(ErasedError::new)? {
            Some(bytes) => {
                let value: U256 = serde_json::from_slice(&bytes).map_err(ErasedError::new)?;
                Ok(value)
            }
            None => Ok(U256::ZERO),
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match db_get::<CallBytecodes>(&self.db, code_hash.as_slice()).map_err(ErasedError::new)? {
            Some(bytes) if !bytes.is_empty() => Ok(Bytecode::new_raw(bytes.into())),
            _ => {
                // Fallback: code may have been stored before the bytecodes table
                // existed, or the hash is unknown. Return empty bytecode.
                Ok(Bytecode::default())
            }
        }
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        match db_get::<CallBlockHashByHeight>(&self.db, &number.to_be_bytes())
            .map_err(ErasedError::new)?
        {
            Some(bytes) if bytes.len() == 32 => Ok(B256::from_slice(&bytes)),
            _ => Ok(B256::ZERO),
        }
    }
}

/// Apply revm state changes directly to MDBX.
///
/// This is used after executing a transaction with [`CacheDB<EvmDb>`] —
/// the revm [`EvmState`] delta is written straight to persistent storage
/// without ever materialising a full in-memory [`InMemoryStateProvider`].
pub fn apply_revm_state_to_mdbx(
    db: &DatabaseEnv,
    revm_state: &revm::state::EvmState,
) -> Result<(), ErasedError> {
    for (addr, account) in revm_state {
        if !account.is_touched() {
            continue;
        }

        if account.is_selfdestructed() {
            // Remove account and all its storage
            let key = addr.as_slice().to_vec();
            call_storage::reth_db::db_del::<CallEvmAccounts>(db, &key).map_err(ErasedError::new)?;
            // Note: we don't have a range-delete for storage; in production
            // this would require iterating all slots for this address.
            continue;
        }

        // Upsert bytecode table (keyed by code_hash)
        if let Some(c) = &account.info.code {
            let code = c.original_bytes();
            if !code.is_empty() {
                let code_hash = c.hash_slow();
                call_storage::reth_db::save_bytecode(db, &code_hash, &code)
                    .map_err(ErasedError::new)?;
            }
        }

        // Upsert account info
        let mut code = alloy_primitives::Bytes::default();
        if let Some(c) = &account.info.code {
            code = c.original_bytes();
        }
        let evm_account = EvmAccount {
            nonce: account.info.nonce,
            balance: account.info.balance,
            code: code.clone(),
            storage: account
                .storage
                .iter()
                .map(|(k, v)| (*k, v.present_value()))
                .collect(),
        };
        let key = addr.as_slice().to_vec();
        let value = serde_json::to_vec(&evm_account).unwrap_or_default();
        call_storage::reth_db::db_put::<CallEvmAccounts>(db, key, value)
            .map_err(ErasedError::new)?;

        // Upsert each storage slot individually
        for (slot, value) in &account.storage {
            let slot_value = value.present_value();
            let mut key = Vec::with_capacity(52);
            key.extend_from_slice(addr.as_slice());
            key.extend_from_slice(&slot.to_be_bytes::<32>());
            let value_bytes = serde_json::to_vec(&slot_value).unwrap_or_default();
            call_storage::reth_db::db_put::<CallEvmStorage>(db, key, value_bytes)
                .map_err(ErasedError::new)?;
        }
    }
    Ok(())
}

// ── Historical state (AccountHistory / StorageHistory) ────────────────

/// Record account and storage changes from a revm delta into history tables.
///
/// For every touched account and storage slot, writes the post-execution
/// value keyed by `(address, block_number)` or `(address, slot, block_number)`.
/// This enables `eth_getBalance(blockTag)` and `eth_getStorageAt(blockTag)`
/// without replaying blocks.
pub fn record_revm_delta_history(
    db: &DatabaseEnv,
    block_number: u64,
    revm_state: &revm::state::EvmState,
) -> Result<(), ErasedError> {
    for (addr, account) in revm_state {
        if !account.is_touched() {
            continue;
        }

        let block_be = block_number.to_be_bytes();

        if account.is_selfdestructed() {
            // Record account destruction with empty value
            let mut key = Vec::with_capacity(28);
            key.extend_from_slice(addr.as_slice());
            key.extend_from_slice(&block_be);
            db_put::<CallAccountHistory>(db, key, vec![]).map_err(ErasedError::new)?;
            continue;
        }

        // Record account info at this block
        let mut code = alloy_primitives::Bytes::default();
        if let Some(c) = &account.info.code {
            code = c.original_bytes();
        }
        let evm_account = EvmAccount {
            nonce: account.info.nonce,
            balance: account.info.balance,
            code: code.clone(),
            storage: account
                .storage
                .iter()
                .map(|(k, v)| (*k, v.present_value()))
                .collect(),
        };
        let mut key = Vec::with_capacity(28);
        key.extend_from_slice(addr.as_slice());
        key.extend_from_slice(&block_be);
        let value = serde_json::to_vec(&evm_account).unwrap_or_default();
        db_put::<CallAccountHistory>(db, key, value).map_err(ErasedError::new)?;

        // Record each changed storage slot
        for (slot, value) in &account.storage {
            let slot_value = value.present_value();
            let mut key = Vec::with_capacity(60);
            key.extend_from_slice(addr.as_slice());
            key.extend_from_slice(&slot.to_be_bytes::<32>());
            key.extend_from_slice(&block_be);
            let value_bytes = serde_json::to_vec(&slot_value).unwrap_or_default();
            db_put::<CallStorageHistory>(db, key, value_bytes).map_err(ErasedError::new)?;
        }
    }
    Ok(())
}

/// Read the historical account state at a specific block number.
///
/// Seeks to the first history entry for `address` with block > `block_number`,
/// then steps back to find the latest entry ≤ `block_number`.
pub fn get_historical_account(
    db: &DatabaseEnv,
    address: Address,
    block_number: u64,
) -> Result<Option<EvmAccount>, ErasedError> {
    let tx = db.tx().map_err(ErasedError::new)?;
    let mut cursor = tx
        .cursor_read::<CallAccountHistory>()
        .map_err(ErasedError::new)?;

    // Seek to the first entry for this address with block > target
    let mut seek_key = Vec::with_capacity(28);
    seek_key.extend_from_slice(address.as_slice());
    seek_key.extend_from_slice(&(block_number + 1).to_be_bytes());

    let found = cursor.seek(seek_key).map_err(ErasedError::new)?;
    // If seek found an entry >= target+1, step back to the one before it.
    // If seek found nothing, the last entry in the table is the candidate.
    let candidate = if found.is_none() {
        cursor.last().map_err(ErasedError::new)?
    } else {
        cursor.prev().map_err(ErasedError::new)?
    };

    if let Some((key, value)) = candidate {
        if key.len() >= 20 && &key[..20] == address.as_slice() {
            if value.is_empty() {
                return Ok(None); // account was destroyed
            }
            let account: EvmAccount = serde_json::from_slice(&value).map_err(ErasedError::new)?;
            return Ok(Some(account));
        }
    }
    Ok(None)
}

/// Read the historical storage slot value at a specific block number.
///
/// Seeks to the first history entry for `(address, slot)` with block > `block_number`,
/// then steps back to find the latest entry ≤ `block_number`.
pub fn get_historical_storage(
    db: &DatabaseEnv,
    address: Address,
    slot: U256,
    block_number: u64,
) -> Result<U256, ErasedError> {
    let tx = db.tx().map_err(ErasedError::new)?;
    let mut cursor = tx
        .cursor_read::<CallStorageHistory>()
        .map_err(ErasedError::new)?;

    let mut seek_key = Vec::with_capacity(60);
    seek_key.extend_from_slice(address.as_slice());
    seek_key.extend_from_slice(&slot.to_be_bytes::<32>());
    seek_key.extend_from_slice(&(block_number + 1).to_be_bytes());

    let found = cursor.seek(seek_key).map_err(ErasedError::new)?;
    let candidate = if found.is_none() {
        cursor.last().map_err(ErasedError::new)?
    } else {
        cursor.prev().map_err(ErasedError::new)?
    };

    if let Some((key, value)) = candidate {
        let prefix_len = 20 + 32;
        if key.len() >= prefix_len
            && &key[..20] == address.as_slice()
            && &key[20..52] == slot.to_be_bytes::<32>()
        {
            let val: U256 = serde_json::from_slice(&value).map_err(ErasedError::new)?;
            return Ok(val);
        }
    }
    Ok(U256::ZERO)
}

/// Prune account history entries older than `cutoff_block`.
///
/// Returns the number of entries pruned.
pub fn prune_account_history(db: &DatabaseEnv, cutoff_block: u64) -> Result<u64, ErasedError> {
    let all = db_iter_all::<CallAccountHistory>(db).map_err(ErasedError::new)?;
    let mut pruned = 0u64;
    for (key, _) in all {
        if key.len() >= 28 {
            let block_num = u64::from_be_bytes(key[20..28].try_into().unwrap_or([0u8; 8]));
            if block_num < cutoff_block {
                db_del::<CallAccountHistory>(db, &key).map_err(ErasedError::new)?;
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

/// Prune storage history entries older than `cutoff_block`.
///
/// Returns the number of entries pruned.
pub fn prune_storage_history(db: &DatabaseEnv, cutoff_block: u64) -> Result<u64, ErasedError> {
    let all = db_iter_all::<CallStorageHistory>(db).map_err(ErasedError::new)?;
    let mut pruned = 0u64;
    for (key, _) in all {
        if key.len() >= 60 {
            let block_num = u64::from_be_bytes(key[52..60].try_into().unwrap_or([0u8; 8]));
            if block_num < cutoff_block {
                db_del::<CallStorageHistory>(db, &key).map_err(ErasedError::new)?;
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

// ── TrieUpdates persistence ───────────────────────────────────────────

/// Save [`TrieUpdates`] to MDBX keyed by block number.
///
/// These updates are produced by reth-trie's `StateRoot::root_with_updates()`
/// and enable incremental state root computation for subsequent blocks.
pub fn save_trie_updates(
    db: &DatabaseEnv,
    block_number: u64,
    updates: &reth_trie::updates::TrieUpdates,
) -> Result<(), ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    let value = serde_json::to_vec(updates).map_err(ErasedError::new)?;
    call_storage::reth_db::db_put::<CallTrieUpdates>(db, key, value).map_err(ErasedError::new)?;
    Ok(())
}

/// Load [`TrieUpdates`] from MDBX for a given block number.
pub fn load_trie_updates(
    db: &DatabaseEnv,
    block_number: u64,
) -> Result<Option<reth_trie::updates::TrieUpdates>, ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    match call_storage::reth_db::db_get::<CallTrieUpdates>(db, &key).map_err(ErasedError::new)? {
        Some(bytes) => {
            let updates = serde_json::from_slice(&bytes).map_err(ErasedError::new)?;
            Ok(Some(updates))
        }
        None => Ok(None),
    }
}

// ── Trie node persistence ───────────────────────────────────────────

/// Apply [`TrieUpdates`] to persistent trie node tables in MDBX.
///
/// Writes account and storage branch nodes to `CallAccountTrie` and
/// `CallStorageTrie` respectively, and removes deleted nodes. This
/// enables `eth_getProof` to read the trie structure from disk instead
/// of recomputing it from scratch.
pub fn apply_trie_updates_to_mdbx(
    db: &DatabaseEnv,
    updates: &reth_trie::updates::TrieUpdates,
) -> Result<(), ErasedError> {
    // Account trie nodes: insert new / updated nodes
    for (nibbles, node) in &updates.account_nodes {
        if nibbles.is_empty() {
            continue;
        }
        let key = nibbles.to_vec();
        let value = serde_json::to_vec(node).map_err(ErasedError::new)?;
        db_put::<CallAccountTrie>(db, key, value).map_err(ErasedError::new)?;
    }

    // Account trie nodes: delete removed nodes
    for nibbles in &updates.removed_nodes {
        if nibbles.is_empty() {
            continue;
        }
        let key = nibbles.to_vec();
        db_del::<CallAccountTrie>(db, &key).map_err(ErasedError::new)?;
    }

    // Storage trie nodes
    for (hashed_address, storage_updates) in &updates.storage_tries {
        if storage_updates.is_deleted {
            // Delete all storage trie nodes for this account
            db_del_prefix::<CallStorageTrie>(db, hashed_address.as_slice())
                .map_err(ErasedError::new)?;
            continue;
        }

        for (nibbles, node) in &storage_updates.storage_nodes {
            if nibbles.is_empty() {
                continue;
            }
            let key = storage_trie_key(*hashed_address, nibbles);
            let value = serde_json::to_vec(node).map_err(ErasedError::new)?;
            db_put::<CallStorageTrie>(db, key, value).map_err(ErasedError::new)?;
        }

        for nibbles in &storage_updates.removed_nodes {
            if nibbles.is_empty() {
                continue;
            }
            let key = storage_trie_key(*hashed_address, nibbles);
            db_del::<CallStorageTrie>(db, &key).map_err(ErasedError::new)?;
        }
    }

    Ok(())
}

/// Build a storage trie key: `[hashed_address: 32 bytes][nibbles...]`.
pub fn storage_trie_key(hashed_address: B256, nibbles: &reth_trie::Nibbles) -> Vec<u8> {
    let mut key = Vec::with_capacity(32 + nibbles.len());
    key.extend_from_slice(hashed_address.as_slice());
    key.extend_from_slice(&nibbles.to_vec());
    key
}

/// Delete all entries in a table whose keys start with the given prefix.
fn db_del_prefix<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    db: &DatabaseEnv,
    prefix: &[u8],
) -> Result<(), ErasedError> {
    let tx = db.tx_mut().map_err(ErasedError::new)?;
    let mut cursor = tx.cursor_write::<T>().map_err(ErasedError::new)?;
    // Seek to the first key >= prefix, then walk forward deleting matches
    let mut current = cursor.seek(prefix.to_vec()).map_err(ErasedError::new)?;
    while let Some((key, _)) = current {
        if key.starts_with(prefix) {
            cursor.delete_current().map_err(ErasedError::new)?;
            current = cursor.next().map_err(ErasedError::new)?;
        } else {
            break;
        }
    }
    tx.commit().map_err(ErasedError::new)?;
    Ok(())
}

// ── Block State Snapshots ────────────────────────────────────────────

/// Save a full `InMemoryStateProvider` snapshot for a specific block number.
///
/// These snapshots enable historical state queries (`eth_getBalance(blockTag)`,
/// `eth_getProof`) without replaying blocks.
pub fn save_block_snapshot(
    db: &DatabaseEnv,
    block_number: u64,
    state: &crate::provider::InMemoryStateProvider,
) -> Result<(), ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    let value = serde_json::to_vec(state).map_err(ErasedError::new)?;
    call_storage::reth_db::db_put::<CallBlockStateSnapshots>(db, key, value)
        .map_err(ErasedError::new)?;
    Ok(())
}

/// Load a full `InMemoryStateProvider` snapshot for a specific block number.
pub fn load_block_snapshot(
    db: &DatabaseEnv,
    block_number: u64,
) -> Result<Option<crate::provider::InMemoryStateProvider>, ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    match call_storage::reth_db::db_get::<CallBlockStateSnapshots>(db, &key)
        .map_err(ErasedError::new)?
    {
        Some(bytes) => {
            let state = serde_json::from_slice(&bytes).map_err(ErasedError::new)?;
            Ok(Some(state))
        }
        None => Ok(None),
    }
}

/// Prune block state snapshots older than the given block number.
///
/// Default retention is 128 blocks. Archive nodes can set `retention_blocks`
/// to `u64::MAX` to keep all snapshots.
pub fn prune_block_snapshots(
    db: &DatabaseEnv,
    current_block: u64,
    retention_blocks: u64,
) -> Result<u64, ErasedError> {
    if retention_blocks == u64::MAX {
        return Ok(0);
    }
    let cutoff = current_block.saturating_sub(retention_blocks);
    let all = call_storage::reth_db::db_iter_all::<CallBlockStateSnapshots>(db)
        .map_err(ErasedError::new)?;
    let mut pruned = 0u64;
    for (key, _) in all {
        if key.len() == 8 {
            let block_num = u64::from_be_bytes(key[..8].try_into().unwrap_or([0u8; 8]));
            if block_num < cutoff {
                call_storage::reth_db::db_del::<CallBlockStateSnapshots>(db, &key)
                    .map_err(ErasedError::new)?;
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::InMemoryStateProvider;
    use crate::{EvmExecutor, EvmTransaction};
    use alloy_primitives::Bytes;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_provider_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("call-evm-db-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = InMemoryStateProvider::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));
        state.set_storage(test_addr(1), U256::from(42), U256::from(123));
        state.set_code(
            test_addr(2),
            Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55]),
        );

        state.save_to_db(&db).expect("save");

        let loaded = InMemoryStateProvider::from_db(&db).expect("load");
        assert_eq!(loaded.get_balance(&test_addr(1)), U256::from(1000));
        assert_eq!(loaded.get_nonce(&test_addr(1)), 1);
        assert_eq!(
            loaded.get_storage(&test_addr(1), U256::from(42)),
            U256::from(123)
        );
        assert_eq!(
            loaded.get_code(&test_addr(2)),
            Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55])
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_basic_ref() {
        let tmp =
            std::env::temp_dir().join(format!("call-evm-db-basic-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = InMemoryStateProvider::new();
        state.set_balance(test_addr(1), U256::from(5000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));

        state.save_to_db(&db).expect("save");

        let evm_db = EvmDb::new(db);
        let info = evm_db
            .basic_ref(test_addr(1))
            .expect("basic_ref")
            .expect("account exists");
        assert_eq!(info.balance, U256::from(5000));
        assert_eq!(info.nonce, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_storage_ref() {
        let tmp =
            std::env::temp_dir().join(format!("call-evm-db-storage-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = InMemoryStateProvider::new();
        state.set_storage(test_addr(3), U256::from(7), U256::from(99));
        state.save_to_db(&db).expect("save");

        let evm_db = EvmDb::new(db);
        let value = evm_db
            .storage_ref(test_addr(3), U256::from(7))
            .expect("storage_ref");
        assert_eq!(value, U256::from(99));

        let missing = evm_db
            .storage_ref(test_addr(3), U256::from(8))
            .expect("storage_ref missing");
        assert_eq!(missing, U256::ZERO);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_block_hash_ref() {
        let tmp =
            std::env::temp_dir().join(format!("call-evm-db-hash-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Save a block hash for height 42
        let expected_hash = B256::from([0xABu8; 32]);
        call_storage::reth_db::save_block_hash_by_height(&db, 42, &expected_hash)
            .expect("save block hash");

        let evm_db = EvmDb::new(db);
        let hash = evm_db.block_hash_ref(42).expect("block_hash_ref");
        assert_eq!(hash, expected_hash);

        // Missing block returns zero
        let missing = evm_db.block_hash_ref(99).expect("block_hash_ref missing");
        assert_eq!(missing, B256::ZERO);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_code_by_hash_ref() {
        let tmp =
            std::env::temp_dir().join(format!("call-evm-db-code-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let code = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55]);
        let code_hash = alloy_primitives::keccak256(&code);

        // Save bytecode via helper
        call_storage::reth_db::save_bytecode(&db, &code_hash, &code).expect("save bytecode");

        let evm_db = EvmDb::new(db);
        let loaded = evm_db
            .code_by_hash_ref(code_hash)
            .expect("code_by_hash_ref");
        assert_eq!(loaded.original_bytes(), code);

        // Unknown hash returns empty bytecode
        let unknown = evm_db
            .code_by_hash_ref(B256::from([0xFFu8; 32]))
            .expect("code_by_hash_ref unknown");
        assert!(unknown.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_execute_via_cachedb_mdbx() {
        let tmp =
            std::env::temp_dir().join(format!("call-evm-cachedb-test-{}", std::process::id()));
        let db_env = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Seed MDBX with an account that has balance
        let mut state = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);
        state.set_balance(caller, U256::from(1_000_000_000i128));
        state.create_account(caller);
        state.save_to_db(&db_env).expect("seed db");

        // Execute a transfer directly against MDBX via CacheDB
        let executor = EvmExecutor::new(1);
        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: Bytes::default(),
            chain_id: 1,
        };

        let evm_db = EvmDb::new(Arc::clone(&db_env));
        let mut cache_db = revm::database::CacheDB::new(evm_db);
        let (result, revm_state) = executor
            .execute_tx_db(tx, &mut cache_db, 0, 0)
            .expect("execute via CacheDB");

        assert!(result.success, "tx failed: gas_used={}", result.gas_used);

        // Apply revm state changes back to MDBX
        apply_revm_state_to_mdbx(&db_env, &revm_state).expect("apply to mdbx");

        // Verify balances in MDBX
        let evm_db2 = EvmDb::new(Arc::clone(&db_env));
        let caller_info = evm_db2.basic_ref(caller).unwrap().unwrap();
        let recipient_info = evm_db2.basic_ref(recipient).unwrap().unwrap();

        // Caller spent 100 value + gas. Exact gas depends on execution,
        // but balance should be less than initial 1B.
        assert!(caller_info.balance < U256::from(1_000_000_000i128));
        assert_eq!(recipient_info.balance, U256::from(100));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_trie_updates_save_load_roundtrip() {
        let tmp =
            std::env::temp_dir().join(format!("call-trie-updates-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Build a small InMemoryStateProvider and compute trie updates
        let mut state = InMemoryStateProvider::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.set_storage(test_addr(2), U256::from(7), U256::from(99));

        let (_root, updates) =
            crate::trie::compute_state_root_with_updates(&state).expect("compute updates");

        // Persist updates for block 5
        save_trie_updates(&db, 5, &updates).expect("save updates");

        // Load back
        let loaded = load_trie_updates(&db, 5).expect("load updates");
        assert!(loaded.is_some(), "trie updates should be persisted");

        let loaded = loaded.unwrap();
        // Verify structural equivalence (serde roundtrip)
        assert_eq!(
            serde_json::to_string(&updates).unwrap(),
            serde_json::to_string(&loaded).unwrap()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_block_snapshot_save_load_prune() {
        let tmp =
            std::env::temp_dir().join(format!("call-block-snapshot-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Create state at block 10
        let mut state = InMemoryStateProvider::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.set_storage(test_addr(1), U256::from(42), U256::from(123));

        // Save snapshot for block 10
        save_block_snapshot(&db, 10, &state).expect("save snapshot");

        // Load it back
        let loaded = load_block_snapshot(&db, 10).expect("load snapshot");
        assert!(loaded.is_some(), "snapshot should exist");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.get_balance(&test_addr(1)), U256::from(1000));
        assert_eq!(
            loaded.get_storage(&test_addr(1), U256::from(42)),
            U256::from(123)
        );

        // Missing snapshot
        let missing = load_block_snapshot(&db, 99).expect("load missing");
        assert!(missing.is_none(), "snapshot should not exist for block 99");

        // Save another snapshot at block 200
        let mut state2 = InMemoryStateProvider::new();
        state2.set_balance(test_addr(2), U256::from(5000));
        save_block_snapshot(&db, 200, &state2).expect("save snapshot 200");

        // Prune snapshots older than 128 blocks from current=200 (cutoff=72)
        let pruned = prune_block_snapshots(&db, 200, 128).expect("prune");
        assert_eq!(pruned, 1, "block 10 should be pruned");

        // Block 10 should be gone
        let pruned_10 = load_block_snapshot(&db, 10).expect("load after prune");
        assert!(pruned_10.is_none(), "block 10 should be pruned");

        // Block 200 should still exist
        let still_200 = load_block_snapshot(&db, 200).expect("load 200");
        assert!(still_200.is_some(), "block 200 should still exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_history_record_and_query_account() {
        let tmp =
            std::env::temp_dir().join(format!("call-history-account-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Simulate revm delta at block 10
        let mut revm_state = revm::state::EvmState::default();
        let addr = test_addr(1);
        let mut account = revm::state::Account::default();
        account.mark_touch();
        account.info.balance = U256::from(1000);
        account.info.nonce = 5;
        revm_state.insert(addr, account);

        record_revm_delta_history(&db, 10, &revm_state).expect("record history");

        // Query at block 10 -> should find the account
        let acc = get_historical_account(&db, addr, 10).expect("query");
        assert!(acc.is_some());
        let acc = acc.unwrap();
        assert_eq!(acc.balance, U256::from(1000));
        assert_eq!(acc.nonce, 5);

        // Query at block 9 -> no history yet
        let acc = get_historical_account(&db, addr, 9).expect("query");
        assert!(acc.is_none());

        // Update at block 20
        let mut revm_state2 = revm::state::EvmState::default();
        let mut account2 = revm::state::Account::default();
        account2.mark_touch();
        account2.info.balance = U256::from(2000);
        account2.info.nonce = 6;
        revm_state2.insert(addr, account2);
        record_revm_delta_history(&db, 20, &revm_state2).expect("record history 2");

        // Query at block 15 -> should return block 10 state
        let acc = get_historical_account(&db, addr, 15).expect("query");
        assert_eq!(acc.unwrap().balance, U256::from(1000));

        // Query at block 20 -> should return block 20 state
        let acc = get_historical_account(&db, addr, 20).expect("query");
        assert_eq!(acc.unwrap().balance, U256::from(2000));

        // Query at block 25 -> should return block 20 state
        let acc = get_historical_account(&db, addr, 25).expect("query");
        assert_eq!(acc.unwrap().balance, U256::from(2000));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_history_record_and_query_storage() {
        let tmp =
            std::env::temp_dir().join(format!("call-history-storage-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let addr = test_addr(1);
        let slot = U256::from(42);

        // Block 10: slot = 100
        let mut revm_state = revm::state::EvmState::default();
        let mut account = revm::state::Account::default();
        account.mark_touch();
        account
            .storage
            .insert(slot, revm::state::EvmStorageSlot::new(U256::from(100), 0));
        revm_state.insert(addr, account);
        record_revm_delta_history(&db, 10, &revm_state).expect("record history");

        // Block 20: slot = 200
        let mut revm_state2 = revm::state::EvmState::default();
        let mut account2 = revm::state::Account::default();
        account2.mark_touch();
        account2
            .storage
            .insert(slot, revm::state::EvmStorageSlot::new(U256::from(200), 0));
        revm_state2.insert(addr, account2);
        record_revm_delta_history(&db, 20, &revm_state2).expect("record history 2");

        // Query at block 9 -> no history
        let val = get_historical_storage(&db, addr, slot, 9).expect("query");
        assert_eq!(val, U256::ZERO);

        // Query at block 10 -> 100
        let val = get_historical_storage(&db, addr, slot, 10).expect("query");
        assert_eq!(val, U256::from(100));

        // Query at block 15 -> 100
        let val = get_historical_storage(&db, addr, slot, 15).expect("query");
        assert_eq!(val, U256::from(100));

        // Query at block 20 -> 200
        let val = get_historical_storage(&db, addr, slot, 20).expect("query");
        assert_eq!(val, U256::from(200));

        // Query at block 25 -> 200
        let val = get_historical_storage(&db, addr, slot, 25).expect("query");
        assert_eq!(val, U256::from(200));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_history_prune() {
        let tmp =
            std::env::temp_dir().join(format!("call-history-prune-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let addr = test_addr(1);
        for block in [1u64, 10, 20, 30] {
            let mut revm_state = revm::state::EvmState::default();
            let mut account = revm::state::Account::default();
            account.mark_touch();
            account.info.balance = U256::from(block);
            revm_state.insert(addr, account);
            record_revm_delta_history(&db, block, &revm_state).expect("record");
        }

        // Prune entries older than block 15
        let pruned = prune_account_history(&db, 15).expect("prune");
        assert_eq!(pruned, 2, "blocks 1 and 10 should be pruned");

        // Block 1 and 10 should be gone -> no history before block 20
        let acc = get_historical_account(&db, addr, 5).expect("query");
        assert!(acc.is_none());
        let acc = get_historical_account(&db, addr, 10).expect("query");
        assert!(acc.is_none());

        // Block 20 should still exist
        let acc = get_historical_account(&db, addr, 20).expect("query");
        assert_eq!(acc.unwrap().balance, U256::from(20));

        // Block 30 should still exist
        let acc = get_historical_account(&db, addr, 30).expect("query");
        assert_eq!(acc.unwrap().balance, U256::from(30));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

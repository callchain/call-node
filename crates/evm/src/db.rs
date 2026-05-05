//! Persistent EVM state backed by MDBX (reth-db).
//!
//! Provides `EvmDb` implementing revm's `DatabaseRef` trait for direct
//! disk-backed execution, plus `EvmState` save/load helpers.

use std::sync::Arc;
use alloy_primitives::{Address, U256, B256};
use reth_db::DatabaseEnv;
use revm::{bytecode::Bytecode, state::AccountInfo};
use revm::database_interface::{DatabaseRef, ErasedError};
use call_storage::reth_db::{db_batch_put, db_clear, db_get, db_iter_all, CallEvmAccounts, CallEvmStorage, CallTrieUpdates, CallBlockStateSnapshots};
use crate::state::{EvmAccount, EvmState};

/// Simple error wrapper for string messages.
#[derive(Debug)]
struct DbError(String);

impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DbError {}

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

    /// Load the full `EvmState` from MDBX.
    pub fn load_state(&self) -> Result<EvmState, ErasedError> {
        EvmState::load_from_db(&self.db)
    }

    /// Save the full `EvmState` to MDBX.
    pub fn save_state(&self, state: &EvmState) -> Result<(), ErasedError> {
        state.save_to_db(&self.db)
    }
}

impl DatabaseRef for EvmDb {
    type Error = ErasedError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let key = address.as_slice().to_vec();
        match db_get::<CallEvmAccounts>(&self.db, &key).map_err(ErasedError::new)? {
            Some(bytes) => {
                let account: EvmAccount = serde_json::from_slice(&bytes)
                    .map_err(ErasedError::new)?;
                let code = if account.code.is_empty() {
                    None
                } else {
                    Some(Bytecode::new_raw(account.code))
                };
                Ok(Some(AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: code.as_ref().map(|c| c.hash_slow()).unwrap_or(revm::primitives::KECCAK_EMPTY),
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
                let value: U256 = serde_json::from_slice(&bytes)
                    .map_err(ErasedError::new)?;
                Ok(value)
            }
            None => Ok(U256::ZERO),
        }
    }

    fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        // Code is always loaded via `basic_ref` (AccountInfo includes the full code).
        // This path should not be hit in normal operation.
        Ok(Bytecode::default())
    }

    fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
        // Block hash is not stored in the EVM DB; return zero.
        Ok(B256::ZERO)
    }
}

/// Apply revm state changes directly to MDBX.
///
/// This is used after executing a transaction with [`CacheDB<EvmDb>`] —
/// the revm [`EvmState`] delta is written straight to persistent storage
/// without ever materialising a full in-memory [`EvmState`].
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
            call_storage::reth_db::db_del::<CallEvmAccounts>(db, &key)
                .map_err(ErasedError::new)?;
            // Note: we don't have a range-delete for storage; in production
            // this would require iterating all slots for this address.
            continue;
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
    call_storage::reth_db::db_put::<CallTrieUpdates>(db, key, value)
        .map_err(ErasedError::new)?;
    Ok(())
}

/// Load [`TrieUpdates`] from MDBX for a given block number.
pub fn load_trie_updates(
    db: &DatabaseEnv,
    block_number: u64,
) -> Result<Option<reth_trie::updates::TrieUpdates>, ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    match call_storage::reth_db::db_get::<CallTrieUpdates>(db, &key)
        .map_err(ErasedError::new)?
    {
        Some(bytes) => {
            let updates = serde_json::from_slice(&bytes).map_err(ErasedError::new)?;
            Ok(Some(updates))
        }
        None => Ok(None),
    }
}

// ── Block State Snapshots ────────────────────────────────────────────

/// Save a full `EvmState` snapshot for a specific block number.
///
/// These snapshots enable historical state queries (`eth_getBalance(blockTag)`,
/// `eth_getProof`) without replaying blocks.
pub fn save_block_snapshot(
    db: &DatabaseEnv,
    block_number: u64,
    state: &EvmState,
) -> Result<(), ErasedError> {
    let key = block_number.to_be_bytes().to_vec();
    let value = serde_json::to_vec(state).map_err(ErasedError::new)?;
    call_storage::reth_db::db_put::<CallBlockStateSnapshots>(db, key, value)
        .map_err(ErasedError::new)?;
    Ok(())
}

/// Load a full `EvmState` snapshot for a specific block number.
pub fn load_block_snapshot(
    db: &DatabaseEnv,
    block_number: u64,
) -> Result<Option<EvmState>, ErasedError> {
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

/// Helpers to persist and load `EvmState` via MDBX.
impl EvmState {
    /// Save all accounts and storage slots to MDBX.
    /// Replaces any existing EVM state in the database.
    pub fn save_to_db(&self, db: &DatabaseEnv) -> Result<(), ErasedError> {
        // Clear existing state
        db_clear::<CallEvmAccounts>(db).map_err(ErasedError::new)?;
        db_clear::<CallEvmStorage>(db).map_err(ErasedError::new)?;

        // Batch-write accounts
        let account_entries: Vec<(Vec<u8>, Vec<u8>)> = self
            .get_all_accounts()
            .iter()
            .map(|(addr, acc)| {
                let key = addr.as_slice().to_vec();
                let value = serde_json::to_vec(acc).unwrap_or_default();
                (key, value)
            })
            .collect();
        if !account_entries.is_empty() {
            db_batch_put::<CallEvmAccounts>(db, account_entries)
                .map_err(ErasedError::new)?;
        }

        // Batch-write storage slots
        let mut storage_entries = Vec::new();
        for (addr, acc) in self.get_all_accounts() {
            for (slot, value) in &acc.storage {
                let mut key = Vec::with_capacity(52);
                key.extend_from_slice(addr.as_slice());
                key.extend_from_slice(&slot.to_be_bytes::<32>());
                let value_bytes = serde_json::to_vec(value).unwrap_or_default();
                storage_entries.push((key, value_bytes));
            }
        }
        if !storage_entries.is_empty() {
            db_batch_put::<CallEvmStorage>(db, storage_entries)
                .map_err(ErasedError::new)?;
        }

        Ok(())
    }

    /// Load all accounts and storage slots from MDBX into a new `EvmState`.
    pub fn load_from_db(db: &DatabaseEnv) -> Result<EvmState, ErasedError> {
        let account_data = db_iter_all::<CallEvmAccounts>(db)
            .map_err(ErasedError::new)?;
        let storage_data = db_iter_all::<CallEvmStorage>(db)
            .map_err(ErasedError::new)?;

        let mut state = EvmState::new();

        for (key, value) in account_data {
            if key.len() != 20 {
                continue;
            }
            let addr = Address::from_slice(&key);
            let account: EvmAccount = serde_json::from_slice(&value)
                .map_err(ErasedError::new)?;
            state.accounts.insert(addr, account);
        }

        for (key, value) in storage_data {
            if key.len() != 52 {
                continue;
            }
            let addr = Address::from_slice(&key[..20]);
            let slot_bytes: [u8; 32] = key[20..52]
                .try_into()
                .map_err(|_| ErasedError::new(DbError("invalid storage key length".into())))?;
            let slot = U256::from_be_bytes(slot_bytes);
            let slot_value: U256 = serde_json::from_slice(&value)
                .map_err(ErasedError::new)?;

            if let Some(acc) = state.accounts.get_mut(&addr) {
                acc.storage.insert(slot, slot_value);
            }
        }

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use crate::{EvmExecutor, EvmTransaction};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_evm_state_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("call-evm-db-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));
        state.set_storage(test_addr(1), U256::from(42), U256::from(123));
        state.set_code(test_addr(2), Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55]));

        state.save_to_db(&db).expect("save");

        let loaded = EvmState::load_from_db(&db).expect("load");
        assert_eq!(loaded.get_balance(&test_addr(1)), U256::from(1000));
        assert_eq!(loaded.get_nonce(&test_addr(1)), 1);
        assert_eq!(loaded.get_storage(&test_addr(1), U256::from(42)), U256::from(123));
        assert_eq!(loaded.get_code(&test_addr(2)), Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55]));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_basic_ref() {
        let tmp = std::env::temp_dir().join(format!("call-evm-db-basic-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(5000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));

        state.save_to_db(&db).expect("save");

        let evm_db = EvmDb::new(db);
        let info = evm_db.basic_ref(test_addr(1)).expect("basic_ref").expect("account exists");
        assert_eq!(info.balance, U256::from(5000));
        assert_eq!(info.nonce, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_db_storage_ref() {
        let tmp = std::env::temp_dir().join(format!("call-evm-db-storage-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut state = EvmState::new();
        state.set_storage(test_addr(3), U256::from(7), U256::from(99));
        state.save_to_db(&db).expect("save");

        let evm_db = EvmDb::new(db);
        let value = evm_db.storage_ref(test_addr(3), U256::from(7)).expect("storage_ref");
        assert_eq!(value, U256::from(99));

        let missing = evm_db.storage_ref(test_addr(3), U256::from(8)).expect("storage_ref missing");
        assert_eq!(missing, U256::ZERO);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_evm_execute_via_cachedb_mdbx() {
        let tmp = std::env::temp_dir().join(format!(
            "call-evm-cachedb-test-{}",
            std::process::id()
        ));
        let db_env = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Seed MDBX with an account that has balance
        let mut state = EvmState::new();
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
        let tmp = std::env::temp_dir().join(format!(
            "call-trie-updates-test-{}",
            std::process::id()
        ));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Build a small EvmState and compute trie updates
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.set_storage(test_addr(2), U256::from(7), U256::from(99));

        let (_root, updates) = crate::trie::compute_state_root_with_updates(&state)
            .expect("compute updates");

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
        let tmp = std::env::temp_dir().join(format!(
            "call-block-snapshot-test-{}",
            std::process::id()
        ));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Create state at block 10
        let mut state = EvmState::new();
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
        assert_eq!(loaded.get_storage(&test_addr(1), U256::from(42)), U256::from(123));

        // Missing snapshot
        let missing = load_block_snapshot(&db, 99).expect("load missing");
        assert!(missing.is_none(), "snapshot should not exist for block 99");

        // Save another snapshot at block 200
        let mut state2 = EvmState::new();
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
}

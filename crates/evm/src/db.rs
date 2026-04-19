//! Persistent EVM state backed by MDBX (reth-db).
//!
//! Provides `EvmDb` implementing revm's `DatabaseRef` trait for direct
//! disk-backed execution, plus `EvmState` save/load helpers.

use std::sync::Arc;
use alloy_primitives::{Address, U256, B256};
use reth_db::DatabaseEnv;
use revm::{bytecode::Bytecode, state::AccountInfo};
use revm::database_interface::{DatabaseRef, ErasedError};
use call_storage::reth_db::{db_batch_put, db_clear, db_get, db_iter_all, CallEvmAccounts, CallEvmStorage};
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
}

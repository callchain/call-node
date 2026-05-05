//! reth `StateProvider` / `StateProviderFactory` implementation backed by MDBX.
//!
//! `InMemoryStateProvider` is the canonical production state container.
//! It loads full state from MDBX into memory and answers all queries without
//! further disk access. Future phases will replace the in-memory delegation
//! with direct MDBX cursor access.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::{Address, BlockNumber, StorageKey, StorageValue, B256, U256, Bytes};
use reth_db::DatabaseEnv;
use reth_primitives_traits::{Account, Bytecode};
use reth_storage_errors::provider::{ProviderError, ProviderResult};
use reth_trie::{
    updates::TrieUpdates,
    HashedPostState, HashedStorage, MultiProof, MultiProofTargets, StorageMultiProof,
    StorageProof, TrieInput,
};
use reth_trie_common::AccountProof;
use revm_database::BundleState;

use reth_storage_api::{
    AccountReader, BlockHashReader, BlockIdReader, BlockNumReader, BytecodeReader,
    HashedPostStateProvider, StateProofProvider, StateProvider, StateProviderBox,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};

use crate::state::EvmAccount;
use crate::trie::{
    compute_state_root_reth, compute_state_root_with_updates,
    compute_account_proof, compute_state_multiproof, to_reth_account,
};
use crate::db::load_block_snapshot;

// ── In-memory StateProvider (production state container) ──────────────

/// The canonical production state container.
///
/// Loads full state from MDBX into memory and implements both reth's
/// `StateProvider` traits and the protocol-level `ProtocolStorage` trait.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InMemoryStateProvider {
    pub(crate) accounts: HashMap<Address, EvmAccount>,
    block_hashes: HashMap<BlockNumber, B256>,
}

impl InMemoryStateProvider {
    /// Create an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a provider from an existing account map.
    pub fn from_accounts(accounts: HashMap<Address, EvmAccount>) -> Self {
        Self {
            accounts,
            block_hashes: HashMap::new(),
        }
    }

    /// Load full state from MDBX.
    pub fn from_db(db: &DatabaseEnv) -> ProviderResult<Self> {
        let account_data = call_storage::reth_db::db_iter_all::<
            call_storage::reth_db::CallEvmAccounts,
        >(db)
        .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(e.to_string())))?;
        let storage_data = call_storage::reth_db::db_iter_all::<
            call_storage::reth_db::CallEvmStorage,
        >(db)
        .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(e.to_string())))?;

        let mut accounts = HashMap::new();

        for (key, value) in account_data {
            if key.len() != 20 {
                continue;
            }
            let addr = Address::from_slice(&key);
            let account: EvmAccount = serde_json::from_slice(&value)
                .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(format!("deserialize account: {e}"))))?;
            accounts.insert(addr, account);
        }

        for (key, value) in storage_data {
            if key.len() != 52 {
                continue;
            }
            let addr = Address::from_slice(&key[..20]);
            let slot_bytes: [u8; 32] = key[20..52].try_into()
                .map_err(|_| ProviderError::Database(reth_db::DatabaseError::Other("invalid storage key".into())))?;
            let slot = U256::from_be_bytes(slot_bytes);
            let slot_value: U256 = serde_json::from_slice(&value)
                .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(format!("deserialize storage: {e}"))))?;

            if let Some(acc) = accounts.get_mut(&addr) {
                acc.storage.insert(slot, slot_value);
            }
        }

        Ok(Self::from_accounts(accounts))
    }

    /// Persist full state to MDBX.
    pub fn save_to_db(&self, db: &DatabaseEnv) -> Result<(), revm::database_interface::ErasedError> {
        use call_storage::reth_db::{db_clear, db_batch_put, CallEvmAccounts, CallEvmStorage};

        db_clear::<CallEvmAccounts>(db).map_err(revm::database_interface::ErasedError::new)?;
        db_clear::<CallEvmStorage>(db).map_err(revm::database_interface::ErasedError::new)?;

        let account_entries: Vec<(Vec<u8>, Vec<u8>)> = self
            .accounts
            .iter()
            .map(|(addr, acc)| {
                let key = addr.as_slice().to_vec();
                let value = serde_json::to_vec(acc).unwrap_or_default();
                (key, value)
            })
            .collect();
        if !account_entries.is_empty() {
            db_batch_put::<CallEvmAccounts>(db, account_entries)
                .map_err(revm::database_interface::ErasedError::new)?;
        }

        let mut storage_entries = Vec::new();
        for (addr, acc) in &self.accounts {
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
                .map_err(revm::database_interface::ErasedError::new)?;
        }

        Ok(())
    }

    /// Attach block hashes for `BlockHashReader`.
    pub fn with_block_hashes(mut self, hashes: HashMap<BlockNumber, B256>) -> Self {
        self.block_hashes = hashes;
        self
    }

    // ── Account accessors ─────────────────────────────────────────────

    pub fn get_account(&self, address: &Address) -> Option<&EvmAccount> {
        self.accounts.get(address)
    }

    pub fn get_account_mut(&mut self, address: &Address) -> &mut EvmAccount {
        self.accounts.entry(*address).or_default()
    }

    pub fn create_account(&mut self, address: Address) -> &mut EvmAccount {
        self.accounts.entry(address).or_default()
    }

    pub fn get_balance(&self, address: &Address) -> U256 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(U256::ZERO)
    }

    pub fn set_balance(&mut self, address: Address, balance: U256) {
        self.accounts.entry(address).or_default().balance = balance;
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.accounts.get(address).map(|a| a.nonce).unwrap_or(0)
    }

    pub fn increment_nonce(&mut self, address: Address) {
        self.accounts.entry(address).or_default().nonce += 1;
    }

    pub fn get_storage(&self, address: &Address, key: U256) -> U256 {
        self.accounts
            .get(address)
            .and_then(|a| a.storage.get(&key))
            .copied()
            .unwrap_or(U256::ZERO)
    }

    pub fn set_storage(&mut self, address: Address, key: U256, value: U256) {
        self.accounts.entry(address).or_default().storage.insert(key, value);
    }

    pub fn set_code(&mut self, address: Address, code: Bytes) {
        self.accounts.entry(address).or_default().code = code;
    }

    pub fn get_code(&self, address: &Address) -> Bytes {
        self.accounts
            .get(address)
            .map(|a| a.code.clone())
            .unwrap_or_default()
    }

    pub fn get_all_accounts(&self) -> &HashMap<Address, EvmAccount> {
        &self.accounts
    }

    pub fn into_accounts(self) -> HashMap<Address, EvmAccount> {
        self.accounts
    }

    /// Apply revm state changes.
    pub fn apply_from_revm_state(&mut self, revm_state: &revm::state::EvmState) {
        for (addr, revm_account) in revm_state {
            let account = self.accounts.entry(*addr).or_default();
            let info = &revm_account.info;
            account.balance = info.balance;
            account.nonce = info.nonce;
            if let Some(code) = &info.code {
                account.code = code.original_bytes();
            }
            for (key, storage_slot) in &revm_account.storage {
                if !storage_slot.present_value.is_zero() {
                    account.storage.insert(*key, storage_slot.present_value);
                }
            }
        }
    }

    /// Compute the Ethereum state trie root.
    pub fn compute_state_root(&self) -> B256 {
        compute_state_root_reth(self)
            .expect("reth-trie state root computation should not fail")
    }

    /// Compute state root and collect trie updates.
    pub fn compute_state_root_with_updates(&self) -> (B256, TrieUpdates) {
        compute_state_root_with_updates(self)
            .expect("reth-trie state root computation should not fail")
    }

    /// Self-reference for backward-compatible call-sites.
    pub fn state(&self) -> &Self {
        self
    }

    /// Mutable self-reference for backward-compatible call-sites.
    pub fn state_mut(&mut self) -> &mut Self {
        self
    }
}

impl BlockHashReader for InMemoryStateProvider {
    fn block_hash(&self,
        number: BlockNumber,
    ) -> ProviderResult<Option<B256>> {
        Ok(self.block_hashes.get(&number).copied())
    }

    fn canonical_hashes_range(
        &self,
        _start: BlockNumber,
        _end: BlockNumber,
    ) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

impl AccountReader for InMemoryStateProvider {
    fn basic_account(&self,
        address: &Address,
    ) -> ProviderResult<Option<Account>> {
        Ok(self.accounts.get(address).map(|acc| to_reth_account(acc)))
    }
}

impl BytecodeReader for InMemoryStateProvider {
    fn bytecode_by_hash(
        &self,
        code_hash: &B256,
    ) -> ProviderResult<Option<Bytecode>> {
        for acc in self.accounts.values() {
            let hash = if acc.code.is_empty() {
                revm::primitives::KECCAK_EMPTY
            } else {
                alloy_primitives::keccak256(&acc.code)
            };
            if &hash == code_hash {
                return Ok(Some(Bytecode::new_raw(acc.code.clone())));
            }
        }
        Ok(None)
    }
}

impl StateRootProvider for InMemoryStateProvider {
    fn state_root(&self, _hashed_state: HashedPostState) -> ProviderResult<B256> {
        compute_state_root_with_updates(self)
            .map(|(root, _)| root)
            .map_err(|e| {
                ProviderError::Database(reth_db::DatabaseError::Other(format!(
                    "state root error: {e:?}"
                )))
            })
    }

    fn state_root_from_nodes(&self,
        _input: TrieInput,
    ) -> ProviderResult<B256> {
        self.state_root(HashedPostState::default())
    }

    fn state_root_with_updates(
        &self,
        _hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        compute_state_root_with_updates(self)
            .map_err(|e| {
                ProviderError::Database(reth_db::DatabaseError::Other(format!(
                    "state root error: {e:?}"
                )))
            })
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        self.state_root_with_updates(HashedPostState::default())
    }
}

impl StorageRootProvider for InMemoryStateProvider {
    fn storage_root(
        &self,
        address: Address,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<B256> {
        let account = self.accounts.get(&address);
        Ok(account.map(|a| a.storage_root()).unwrap_or_else(|| {
            // Ethereum empty trie root
            B256::new([
                0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6,
                0x92, 0xc0, 0xf8, 0x6e, 0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0,
                0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
            ])
        }))
    }

    fn storage_proof(
        &self,
        _address: Address,
        _slot: B256,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Ok(StorageProof::default())
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Ok(StorageMultiProof::empty())
    }
}

impl StateProofProvider for InMemoryStateProvider {
    fn proof(
        &self,
        _input: TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        compute_account_proof(self, address, slots)
            .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(format!("proof error: {e:?}"))))
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        compute_state_multiproof(self, targets)
            .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(format!("multiproof error: {e:?}"))))
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
    ) -> ProviderResult<Vec<alloy_primitives::Bytes>> {
        Ok(Vec::new())
    }
}

impl HashedPostStateProvider for InMemoryStateProvider {
    fn hashed_post_state(&self,
        bundle_state: &BundleState,
    ) -> HashedPostState {
        crate::trie::hashed_post_state_from_bundle_state(bundle_state)
    }
}

impl StateProvider for InMemoryStateProvider {
    fn storage(
        &self,
        account: Address,
        storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        let value = self.get_storage(&account, storage_key.into());
        Ok(Some(value))
    }
}

impl revm::database_interface::Database for &mut InMemoryStateProvider {
    type Error = std::convert::Infallible;

    fn basic(&mut self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        Ok(self.accounts.get(&address).map(|account| {
            let code = if account.code.is_empty() {
                None
            } else {
                Some(revm::bytecode::Bytecode::new_raw(account.code.clone()))
            };
            revm::state::AccountInfo {
                balance: account.balance,
                nonce: account.nonce,
                code_hash: code
                    .as_ref()
                    .map(|c| c.hash_slow())
                    .unwrap_or(revm::primitives::KECCAK_EMPTY),
                code,
                account_id: None,
            }
        }))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        for account in self.accounts.values() {
            let hash = if account.code.is_empty() {
                revm::primitives::KECCAK_EMPTY
            } else {
                alloy_primitives::keccak256(&account.code)
            };
            if hash == code_hash {
                return Ok(revm::bytecode::Bytecode::new_raw(account.code.clone()));
            }
        }
        Ok(revm::bytecode::Bytecode::default())
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        Ok(self.get_storage(&address, index))
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

// ── StateProviderFactory ──────────────────────────────────────────────

/// Factory for creating [`StateProvider`] instances at different block heights.
#[derive(Debug, Clone)]
pub struct CallchainStateProviderFactory {
    db: Arc<DatabaseEnv>,
}

impl CallchainStateProviderFactory {
    pub fn new(db: Arc<DatabaseEnv>) -> Self {
        Self { db }
    }
}

impl BlockHashReader for CallchainStateProviderFactory {
    fn block_hash(&self, _number: BlockNumber) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(
        &self,
        _start: BlockNumber,
        _end: BlockNumber,
    ) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

impl BlockNumReader for CallchainStateProviderFactory {
    fn chain_info(&self) -> ProviderResult<reth_chainspec::ChainInfo> {
        Ok(reth_chainspec::ChainInfo {
            best_hash: B256::ZERO,
            best_number: 0,
        })
    }

    fn best_block_number(&self) -> ProviderResult<BlockNumber> {
        Ok(0)
    }

    fn last_block_number(&self) -> ProviderResult<BlockNumber> {
        Ok(0)
    }

    fn block_number(&self, _hash: B256) -> ProviderResult<Option<BlockNumber>> {
        Ok(None)
    }
}

impl BlockIdReader for CallchainStateProviderFactory {
    fn pending_block_num_hash(&self,
    ) -> ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self,
    ) -> ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }

    fn finalized_block_num_hash(
        &self,
    ) -> ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }
}

impl StateProviderFactory for CallchainStateProviderFactory {
    fn latest(&self,
    ) -> ProviderResult<StateProviderBox> {
        let provider = InMemoryStateProvider::from_db(&self.db)?;
        Ok(Box::new(provider))
    }

    fn history_by_block_number(
        &self,
        block_number: BlockNumber,
    ) -> ProviderResult<StateProviderBox> {
        match load_block_snapshot(&self.db, block_number) {
            Ok(Some(provider)) => {
                let provider = provider.with_block_hashes(HashMap::new());
                Ok(Box::new(provider))
            }
            Ok(None) => {
                self.latest()
            }
            Err(e) => Err(ProviderError::Database(reth_db::DatabaseError::Other(
                format!("snapshot load error: {e:?}")
            ))),
        }
    }

    fn history_by_block_hash(
        &self,
        _block_hash: B256,
    ) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn state_by_block_id(
        &self,
        _block_id: alloy_eips::BlockId,
    ) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn pending_state_by_hash(
        &self,
        _block_hash: B256,
    ) -> ProviderResult<Option<Box<dyn StateProvider + Send + 'static>>> {
        Ok(None)
    }

    fn state_by_block_number_or_tag(
        &self,
        _number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn state_by_block_hash(&self, _block: B256) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        Ok(None)
    }
}

// ── EvmAccount storage root helper ────────────────────────────────────

trait StorageRoot {
    fn storage_root(&self) -> B256;
}

impl StorageRoot for EvmAccount {
    fn storage_root(&self) -> B256 {
        if self.storage.is_empty() {
            return B256::new([
                0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6,
                0x92, 0xc0, 0xf8, 0x6e, 0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0,
                0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
            ]);
        }
        let mut hb = alloy_trie::HashBuilder::default();
        let mut slots: Vec<_> = self
            .storage
            .iter()
            .map(|(k, v)| (alloy_primitives::keccak256(k.to_be_bytes::<32>()), v))
            .collect();
        slots.sort_by_key(|(hash, _)| *hash);
        for (hash, value) in slots {
            let path = reth_trie_common::Nibbles::unpack(hash);
            let mut value_rlp = Vec::new();
            alloy_rlp::Encodable::encode(value, &mut value_rlp);
            hb.add_leaf(path, &value_rlp);
        }
        hb.root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_in_memory_provider_reads() {
        let mut provider = InMemoryStateProvider::new();
        provider.set_balance(test_addr(1), U256::from(1000));
        provider.create_account(test_addr(1));
        provider.increment_nonce(test_addr(1));
        provider.set_storage(test_addr(1), U256::from(42), U256::from(123));

        let acc = provider.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(acc.balance, U256::from(1000));
        assert_eq!(acc.nonce, 1);

        let storage = provider.storage(test_addr(1), U256::from(42).into()).unwrap();
        assert_eq!(storage, Some(U256::from(123)));

        assert!(provider.basic_account(&test_addr(0xFF)).unwrap().is_none());
    }

    #[test]
    fn test_state_root_provider() {
        let mut provider = InMemoryStateProvider::new();
        provider.set_balance(test_addr(1), U256::from(100));
        provider.create_account(test_addr(1));

        let root = provider.state_root(HashedPostState::default()).unwrap();
        assert!(!root.is_zero());
    }

    #[test]
    fn test_factory_latest() {
        let tmp = std::env::temp_dir().join(format!(
            "call-provider-factory-test-{}",
            std::process::id()
        ));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut provider = InMemoryStateProvider::new();
        provider.set_balance(test_addr(1), U256::from(5000));
        provider.create_account(test_addr(1));
        provider.save_to_db(&db).expect("seed");

        let factory = CallchainStateProviderFactory::new(db);
        let provider = factory.latest().expect("latest provider");

        let acc = provider.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(acc.balance, U256::from(5000));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_factory_history_by_block_number() {
        let tmp = std::env::temp_dir().join(format!(
            "call-provider-history-test-{}",
            std::process::id()
        ));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        let mut snapshot = InMemoryStateProvider::new();
        snapshot.set_balance(test_addr(1), U256::from(7777));
        snapshot.create_account(test_addr(1));
        crate::db::save_block_snapshot(&db, 5, &snapshot).expect("save snapshot");

        let mut current = InMemoryStateProvider::new();
        current.set_balance(test_addr(1), U256::from(1111));
        current.create_account(test_addr(1));
        current.save_to_db(&db).expect("seed current");

        let factory = CallchainStateProviderFactory::new(db);

        let latest = factory.latest().expect("latest provider");
        let latest_acc = latest.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(latest_acc.balance, U256::from(1111));

        let hist = factory.history_by_block_number(5).expect("historical provider");
        let hist_acc = hist.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(hist_acc.balance, U256::from(7777));

        let missing = factory.history_by_block_number(99).expect("fallback provider");
        let missing_acc = missing.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(missing_acc.balance, U256::from(1111));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

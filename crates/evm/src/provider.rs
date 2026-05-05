//! reth `StateProvider` / `StateProviderFactory` implementation backed by MDBX.
//!
//! This is an incremental migration step: the provider loads the full `EvmState`
//! from MDBX into memory and delegates all reads to it. Future phases will
//! replace the in-memory delegation with direct MDBX cursor access.

use std::sync::Arc;

use alloy_primitives::{Address, BlockNumber, StorageKey, StorageValue, B256};
use reth_db::DatabaseEnv;
use reth_primitives_traits::{Account, Bytecode};
use reth_storage_errors::provider::{ProviderError, ProviderResult};
use reth_trie::{
    updates::TrieUpdates,
    HashedPostState, HashedStorage, MultiProof, MultiProofTargets, StorageMultiProof,
    StorageProof, TrieInput,
};
use reth_trie_common::{AccountProof, Nibbles};
use revm_database::BundleState;

use reth_storage_api::{
    AccountReader, BlockHashReader, BlockIdReader, BlockNumReader, BytecodeReader,
    HashedPostStateProvider, StateProofProvider, StateProvider, StateProviderBox,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};

use crate::state::{EvmAccount, EvmState};
use crate::trie::{compute_state_root_with_updates, to_reth_account};
use crate::db::load_block_snapshot;

// ── In-memory StateProvider (delegates to loaded EvmState) ────────────

/// A [`StateProvider`] that reads from an in-memory [`EvmState`] snapshot.
///
/// This is used as the `latest()` provider: load full state from MDBX once,
/// then answer all queries from memory without further disk access.
#[derive(Debug, Clone)]
pub struct InMemoryStateProvider {
    state: EvmState,
    block_hashes: std::collections::HashMap<BlockNumber, B256>,
}

impl InMemoryStateProvider {
    /// Create a provider from a loaded [`EvmState`].
    pub fn new(state: EvmState) -> Self {
        Self {
            state,
            block_hashes: std::collections::HashMap::new(),
        }
    }

    /// Create a provider from MDBX by loading the full `EvmState`.
    pub fn from_db(db: &DatabaseEnv) -> ProviderResult<Self> {
        let state = EvmState::load_from_db(db)
            .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(e.to_string())))?;
        Ok(Self::new(state))
    }

    /// Attach block hashes for `BlockHashReader`.
    pub fn with_block_hashes(mut self, hashes: std::collections::HashMap<BlockNumber, B256>) -> Self {
        self.block_hashes = hashes;
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
        // Not needed for basic operation; return empty.
        Ok(Vec::new())
    }
}

impl AccountReader for InMemoryStateProvider {
    fn basic_account(&self,
        address: &Address,
    ) -> ProviderResult<Option<Account>> {
        Ok(self.state.get_account(address).map(|acc| to_reth_account(acc)))
    }
}

impl BytecodeReader for InMemoryStateProvider {
    fn bytecode_by_hash(
        &self,
        code_hash: &B256,
    ) -> ProviderResult<Option<Bytecode>> {
        // Scan all accounts to find matching code hash.
        for acc in self.state.accounts.values() {
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
        compute_state_root_with_updates(&self.state)
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
        compute_state_root_with_updates(&self.state)
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
        let account = self.state.get_account(&address);
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
        // Phase 3: implement real storage proof generation
        Ok(StorageProof::default())
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        // Phase 3: implement real storage multiproof
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
        crate::trie::compute_account_proof(&self.state, address, slots)
            .map_err(|e| ProviderError::Database(reth_db::DatabaseError::Other(format!("proof error: {e:?}"))))
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        crate::trie::compute_state_multiproof(&self.state, targets)
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
        let value = self.state.get_storage(&account, storage_key.into());
        Ok(Some(value))
    }
}

// ── StateProviderFactory ──────────────────────────────────────────────

/// Factory for creating [`StateProvider`] instances at different block heights.
///
/// Currently only supports `latest()` (loads full state from MDBX).
/// `history_by_block_number` will be implemented in Phase 3 (historical queries).
#[derive(Debug, Clone)]
pub struct CallchainStateProviderFactory {
    db: Arc<DatabaseEnv>,
}

impl CallchainStateProviderFactory {
    /// Create a new factory backed by the given MDBX environment.
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
        // Try to load a full state snapshot for the requested block.
        match load_block_snapshot(&self.db, block_number) {
            Ok(Some(state)) => {
                let mut provider = InMemoryStateProvider::new(state);
                // Attach empty block hashes — historical queries rarely need them.
                provider = provider.with_block_hashes(std::collections::HashMap::new());
                Ok(Box::new(provider))
            }
            Ok(None) => {
                // No snapshot available — fall back to latest.
                // This happens for pruned blocks or blocks before snapshotting began.
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

// ── EvmAccount storage root helper (reused from state.rs) ─────────────

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
            let path = Nibbles::unpack(hash);
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
    use alloy_primitives::{Address, U256};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_in_memory_provider_reads() {
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));
        state.set_storage(test_addr(1), U256::from(42), U256::from(123));

        let provider = InMemoryStateProvider::new(state);

        // AccountReader
        let acc = provider.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(acc.balance, U256::from(1000));
        assert_eq!(acc.nonce, 1);

        // StateProvider::storage
        let storage = provider.storage(test_addr(1), U256::from(42).into()).unwrap();
        assert_eq!(storage, Some(U256::from(123)));

        // Missing account
        assert!(provider.basic_account(&test_addr(0xFF)).unwrap().is_none());
    }

    #[test]
    fn test_state_root_provider() {
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(100));
        state.create_account(test_addr(1));

        let provider = InMemoryStateProvider::new(state);
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

        // Seed MDBX
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(5000));
        state.create_account(test_addr(1));
        state.save_to_db(&db).expect("seed");

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

        // Save a snapshot for block 5
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(7777));
        state.create_account(test_addr(1));
        crate::db::save_block_snapshot(&db, 5, &state).expect("save snapshot");

        // Also seed current state with different balance
        let mut current = EvmState::new();
        current.set_balance(test_addr(1), U256::from(1111));
        current.create_account(test_addr(1));
        current.save_to_db(&db).expect("seed current");

        let factory = CallchainStateProviderFactory::new(db);

        // latest() should return current state (balance 1111)
        let latest = factory.latest().expect("latest provider");
        let latest_acc = latest.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(latest_acc.balance, U256::from(1111));

        // history_by_block_number(5) should return snapshot state (balance 7777)
        let hist = factory.history_by_block_number(5).expect("historical provider");
        let hist_acc = hist.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(hist_acc.balance, U256::from(7777));

        // history_by_block_number(99) with no snapshot should fall back to latest
        let missing = factory.history_by_block_number(99).expect("fallback provider");
        let missing_acc = missing.basic_account(&test_addr(1)).unwrap().unwrap();
        assert_eq!(missing_acc.balance, U256::from(1111));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

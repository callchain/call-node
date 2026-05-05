//! reth-trie integration for incremental state root computation.
//!
//! Replaces the O(n) `HashBuilder` full aggregation in `EvmState::compute_state_root()`
//! with reth-trie's `StateRoot` which supports incremental updates via `TrieUpdates`.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, map::B256Map, Address, B256, U256};
use reth_primitives_traits::Account;
use reth_storage_errors::db::DatabaseError;
use reth_trie::{
    hashed_cursor::{HashedCursor, HashedCursorFactory, HashedStorageCursor},
    proof::Proof,
    trie_cursor::noop::NoopTrieCursorFactory,
    updates::TrieUpdates,
    StateRoot,
};

use crate::state::{EvmAccount, EvmState};

// ── HashedCursorFactory backed by EvmState ────────────────────────────

/// A [`HashedCursorFactory`] that reads accounts and storage directly from
/// an in-memory [`EvmState`].
///
/// This is used to feed the full state into reth-trie's [`StateRoot`]
/// calculator when computing the state root from scratch (e.g. for the
/// genesis block or when no prior trie updates are persisted).
#[derive(Debug, Clone)]
pub struct EvmStateHashedCursorFactory<'a> {
    state: &'a EvmState,
}

impl<'a> EvmStateHashedCursorFactory<'a> {
    /// Create a new factory from the given [`EvmState`].
    pub fn new(state: &'a EvmState) -> Self {
        Self { state }
    }

    /// Build a sorted `BTreeMap` of hashed address -> reth `Account`.
    fn hashed_accounts(&self) -> BTreeMap<B256, Account> {
        let mut map = BTreeMap::new();
        for (addr, acc) in &self.state.accounts {
            let hashed = keccak256(*addr);
            let account = Account {
                nonce: acc.nonce,
                balance: acc.balance,
                bytecode_hash: if acc.code.is_empty() {
                    None
                } else {
                    Some(keccak256(&acc.code))
                },
            };
            map.insert(hashed, account);
        }
        map
    }

    /// Build a sorted `BTreeMap` of hashed address -> (hashed slot -> value).
    fn hashed_storages(&self) -> B256Map<BTreeMap<B256, U256>> {
        let mut map: B256Map<BTreeMap<B256, U256>> = B256Map::default();
        for (addr, acc) in &self.state.accounts {
            let hashed_addr = keccak256(*addr);
            let mut storage = BTreeMap::new();
            for (slot, value) in &acc.storage {
                if !value.is_zero() {
                    let hashed_slot = keccak256(slot.to_be_bytes::<32>());
                    storage.insert(hashed_slot, *value);
                }
            }
            // Ensure every account has at least an empty storage entry so the
            // cursor factory can answer `hashed_storage_cursor` for any account.
            map.insert(hashed_addr, storage);
        }
        map
    }
}

impl<'a> HashedCursorFactory for EvmStateHashedCursorFactory<'a> {
    type AccountCursor<'b>
        = BTreeHashedCursor<Account>
    where
        Self: 'b;

    type StorageCursor<'b>
        = BTreeHashedCursor<U256>
    where
        Self: 'b;

    fn hashed_account_cursor(&self) -> Result<Self::AccountCursor<'_>, DatabaseError> {
        Ok(BTreeHashedCursor::new(self.hashed_accounts()))
    }

    fn hashed_storage_cursor(
        &self,
        hashed_address: B256,
    ) -> Result<Self::StorageCursor<'_>, DatabaseError> {
        let storages = self.hashed_storages();
        let storage = storages
            .get(&hashed_address)
            .cloned()
            .unwrap_or_default();
        Ok(BTreeHashedCursor::new(storage))
    }
}

// ── Simple BTreeMap-based HashedCursor ────────────────────────────────

/// A generic [`HashedCursor`] backed by a `BTreeMap<B256, V>`.
#[derive(Debug)]
pub struct BTreeHashedCursor<V> {
    data: BTreeMap<B256, V>,
    current_key: Option<B256>,
}

impl<V: Clone + std::fmt::Debug> BTreeHashedCursor<V> {
    fn new(data: BTreeMap<B256, V>) -> Self {
        Self { data, current_key: None }
    }
}

impl<V: Clone + std::fmt::Debug> HashedCursor for BTreeHashedCursor<V> {
    type Value = V;

    fn seek(&mut self, key: B256) -> Result<Option<(B256, Self::Value)>, DatabaseError> {
        let entry = self
            .data
            .iter()
            .find_map(|(k, v)| (k >= &key).then(|| (*k, v.clone())));
        self.current_key = entry.as_ref().map(|(k, _)| *k);
        Ok(entry)
    }

    fn next(&mut self) -> Result<Option<(B256, Self::Value)>, DatabaseError> {
        let mut iter = self.data.iter();
        // Position at current key
        iter.find(|(k, _)| self.current_key.as_ref().is_none_or(|curr| *k >= curr));
        // Get next entry
        let entry = iter.next().map(|(k, v)| (*k, v.clone()));
        self.current_key = entry.as_ref().map(|(k, _)| *k);
        Ok(entry)
    }

    fn reset(&mut self) {
        self.current_key = None;
    }
}

impl HashedStorageCursor for BTreeHashedCursor<U256> {
    fn is_storage_empty(&mut self) -> Result<bool, DatabaseError> {
        Ok(self.data.is_empty())
    }

    fn set_hashed_address(&mut self, _hashed_address: B256) {
        self.reset();
    }
}

// ── State Root Computation ────────────────────────────────────────────

/// Compute the Ethereum state root using reth-trie from the full [`EvmState`].
///
/// This performs a full trie computation (no incremental overlay) and is
/// suitable when:
/// - Computing the genesis state root
/// - No prior `TrieUpdates` are available
/// - Verifying correctness against the legacy `HashBuilder` path
///
/// # Returns
/// The state root hash.
pub fn compute_state_root_reth(state: &EvmState) -> Result<B256, reth_execution_errors::StateRootError> {
    let factory = EvmStateHashedCursorFactory::new(state);
    let state_root = StateRoot::new(NoopTrieCursorFactory::default(), factory);
    state_root.root()
}

/// Compute the Ethereum state root **and** collect [`TrieUpdates`].
///
/// The returned [`TrieUpdates`] can be persisted to MDBX and later used
/// with [`InMemoryTrieCursorFactory`] for incremental computation.
pub fn compute_state_root_with_updates(
    state: &EvmState,
) -> Result<(B256, TrieUpdates), reth_execution_errors::StateRootError> {
    let factory = EvmStateHashedCursorFactory::new(state);
    let state_root = StateRoot::new(NoopTrieCursorFactory::default(), factory);
    state_root.root_with_updates()
}

// ── Proof Generation ──────────────────────────────────────────────────

/// Compute an Ethereum account proof (including storage proofs) from the
/// full [`EvmState`].
///
/// This builds the trie from scratch using the in-memory state and extracts
/// the Merkle proof nodes for the given address and slots.
pub fn compute_account_proof(
    state: &EvmState,
    address: Address,
    slots: &[B256],
) -> Result<reth_trie_common::AccountProof, reth_execution_errors::trie::StateProofError> {
    let factory = EvmStateHashedCursorFactory::new(state);
    let proof = Proof::new(NoopTrieCursorFactory::default(), factory);
    proof.account_proof(address, slots)
}

/// Compute a state multiproof from the full [`EvmState`].
pub fn compute_state_multiproof(
    state: &EvmState,
    targets: reth_trie::MultiProofTargets,
) -> Result<reth_trie::MultiProof, reth_execution_errors::trie::StateProofError> {
    let factory = EvmStateHashedCursorFactory::new(state);
    let proof = Proof::new(NoopTrieCursorFactory::default(), factory);
    proof.multiproof(targets)
}

// ── Helpers: Build HashedPostState from revm changes ──────────────────

/// Build a [`reth_trie::HashedPostState`] from an [`EvmState`] delta.
///
/// This hashes all addresses and storage slots with keccak256, producing
/// the format expected by reth-trie's overlay cursors.
pub fn hashed_post_state_from_evm_state(state: &EvmState) -> reth_trie::HashedPostState {
    use reth_trie::HashedStorage;

    let mut accounts = B256Map::default();
    let mut storages = B256Map::default();

    for (addr, acc) in &state.accounts {
        let hashed_addr = keccak256(*addr);

        let account = Account {
            nonce: acc.nonce,
            balance: acc.balance,
            bytecode_hash: if acc.code.is_empty() {
                None
            } else {
                Some(keccak256(&acc.code))
            },
        };
        accounts.insert(hashed_addr, Some(account));

        let mut storage_map = B256Map::default();
        for (slot, value) in &acc.storage {
            let hashed_slot = keccak256(slot.to_be_bytes::<32>());
            storage_map.insert(hashed_slot, *value);
        }
        storages.insert(hashed_addr, HashedStorage::from_iter(false, storage_map));
    }

    reth_trie::HashedPostState { accounts, storages }
}

/// Build a [`reth_trie::HashedPostState`] from a revm [`EvmState`] delta.
///
/// This is the typical entry point after executing a transaction:
/// the revm `EvmState` contains only touched accounts with their final values.
pub fn hashed_post_state_from_revm_state(
    revm_state: &revm::state::EvmState,
) -> reth_trie::HashedPostState {
    use reth_trie::HashedStorage;

    let mut accounts = B256Map::default();
    let mut storages = B256Map::default();

    for (addr, acc) in revm_state {
        // revm's EvmState only contains touched accounts.
        // Skip untouched accounts entirely.
        if !acc.is_touched() {
            continue;
        }

        let hashed_addr = keccak256(*addr);

        if acc.is_selfdestructed() {
            accounts.insert(hashed_addr, None);
        } else {
            let account = Account::from_revm_account(acc);
            accounts.insert(hashed_addr, Some(account));

            let mut storage_map = B256Map::default();
            for (slot, value) in &acc.storage {
                let hashed_slot = keccak256(slot.to_be_bytes::<32>());
                storage_map.insert(hashed_slot, value.present_value());
            }
            storages.insert(
                hashed_addr,
                HashedStorage::from_iter(acc.is_selfdestructed(), storage_map),
            );
        }
    }

    reth_trie::HashedPostState { accounts, storages }
}

/// Build a [`reth_trie::HashedPostState`] from a revm [`BundleState`].
///
/// This is the entry point after executing a block via revm's
/// `CacheDB` + `Evm::transact()`: the [`BundleState`] contains only
/// touched accounts with their final values.
pub fn hashed_post_state_from_bundle_state(
    bundle: &revm_database::BundleState,
) -> reth_trie::HashedPostState {
    use reth_trie::HashedStorage;

    let mut accounts = B256Map::default();
    let mut storages = B256Map::default();

    for (addr, acc) in &bundle.state {
        // Skip accounts that were merely loaded and never modified.
        if acc.status.is_not_modified() {
            continue;
        }

        let hashed_addr = keccak256(*addr);

        if acc.status.was_destroyed() {
            accounts.insert(hashed_addr, None);
        } else {
            let info = acc.info.as_ref().expect("modified account has info");
            let account = Account {
                nonce: info.nonce,
                balance: info.balance,
                bytecode_hash: if info.code_hash == revm::primitives::KECCAK_EMPTY {
                    None
                } else {
                    Some(info.code_hash)
                },
            };
            accounts.insert(hashed_addr, Some(account));

            let mut storage_map = B256Map::default();
            for (slot, value) in &acc.storage {
                let hashed_slot = keccak256(slot.to_be_bytes::<32>());
                storage_map.insert(hashed_slot, value.present_value);
            }
            storages.insert(
                hashed_addr,
                HashedStorage::from_iter(false, storage_map),
            );
        }
    }

    reth_trie::HashedPostState { accounts, storages }
}

// ── Conversion helpers ────────────────────────────────────────────────

/// Convert an [`EvmAccount`] to a reth [`Account`].
pub fn to_reth_account(acc: &EvmAccount) -> Account {
    Account {
        nonce: acc.nonce,
        balance: acc.balance,
        bytecode_hash: if acc.code.is_empty() {
            None
        } else {
            Some(keccak256(&acc.code))
        },
    }
}

/// Convert a reth [`Account`] to an [`EvmAccount`] (code is left empty;
/// must be filled separately if needed).
pub fn from_reth_account(acc: Account) -> EvmAccount {
    EvmAccount {
        nonce: acc.nonce,
        balance: acc.balance,
        code: alloy_primitives::Bytes::default(),
        storage: std::collections::HashMap::new(),
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
    fn test_reth_state_root_matches_legacy() {
        let mut state = EvmState::new();

        // Single account with balance
        state.set_balance(test_addr(1), U256::from(1000));
        state.create_account(test_addr(1));
        state.increment_nonce(test_addr(1));

        // Account with code
        state.set_code(test_addr(2), alloy_primitives::Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0x55]));

        // Account with storage
        state.set_storage(test_addr(3), U256::from(1), U256::from(42));
        state.set_storage(test_addr(3), U256::from(2), U256::from(99));

        let legacy_root = state.compute_state_root_legacy();
        let reth_root = compute_state_root_reth(&state).expect("reth root computation");

        assert_eq!(
            legacy_root, reth_root,
            "reth-trie state root must match legacy HashBuilder implementation"
        );
    }

    #[test]
    fn test_reth_state_root_with_updates() {
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(500));
        state.create_account(test_addr(1));
        state.set_storage(test_addr(2), U256::from(7), U256::from(123));

        let (root, updates) = compute_state_root_with_updates(&state).expect("updates");
        assert!(!root.is_zero());
        // TrieUpdates should contain at least some account nodes for non-trivial state
        assert!(!updates.account_nodes.is_empty() || !updates.storage_tries.is_empty());
    }

    #[test]
    fn test_reth_state_root_empty_state() {
        let state = EvmState::new();
        let root = compute_state_root_reth(&state).expect("empty root");
        assert_eq!(root, alloy_primitives::B256::new([
            0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6,
            0x92, 0xc0, 0xf8, 0x6e, 0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0,
            0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
        ]));
    }

    #[test]
    fn test_hashed_post_state_from_evm_state() {
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(100));
        state.increment_nonce(test_addr(1));
        state.set_storage(test_addr(1), U256::from(1), U256::from(42));

        let hashed = hashed_post_state_from_evm_state(&state);
        let hashed_addr = keccak256(test_addr(1));

        assert!(hashed.accounts.contains_key(&hashed_addr));
        assert!(hashed.storages.contains_key(&hashed_addr));

        let account = hashed.accounts.get(&hashed_addr).unwrap().unwrap();
        assert_eq!(account.nonce, 1);
        assert_eq!(account.balance, U256::from(100));
    }
}

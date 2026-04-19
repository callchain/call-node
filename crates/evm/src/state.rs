//! EVM state management (accounts, contracts, storage).

use call_primitives::Address;
use alloy_primitives::{U256, Bytes, keccak256, B256};
use alloy_trie::{HashBuilder, Nibbles};
use alloy_rlp::Encodable;
use revm::{
    database::InMemoryDB,
    state::AccountInfo,
    bytecode::Bytecode,
};
use std::collections::HashMap;

/// Ethereum empty trie root hash (keccak256 of RLP empty string).
const EMPTY_ROOT: B256 = B256::new([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6,
    0x92, 0xc0, 0xf8, 0x6e, 0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0,
    0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

/// EVM account info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvmAccount {
    pub nonce: u64,
    pub balance: U256,
    pub code: Bytes,
    pub storage: HashMap<U256, U256>,
}

impl Default for EvmAccount {
    fn default() -> Self {
        Self {
            nonce: 0,
            balance: U256::ZERO,
            code: Bytes::default(),
            storage: HashMap::new(),
        }
    }
}

impl EvmAccount {
    /// Compute the storage trie root for this account.
    fn storage_root(&self) -> B256 {
        if self.storage.is_empty() {
            return EMPTY_ROOT;
        }
        let mut hb = HashBuilder::default();
        let mut slots: Vec<_> = self.storage.iter().collect();
        slots.sort_by_key(|(k, _)| *k);
        for (key, value) in slots {
            let path = Nibbles::unpack(keccak256(key.to_be_bytes::<32>()));
            let mut value_rlp = Vec::new();
            value.encode(&mut value_rlp);
            hb.add_leaf(path, &value_rlp);
        }
        hb.root()
    }

    /// RLP-encode the account as `[nonce, balance, storage_root, code_hash]`.
    fn rlp_encode(&self, storage_root: B256) -> Vec<u8> {
        let code_hash = if self.code.is_empty() {
            revm::primitives::KECCAK_EMPTY
        } else {
            keccak256(&self.code)
        };

        // Encode payload elements first to measure length
        let mut payload = Vec::new();
        self.nonce.encode(&mut payload);
        self.balance.encode(&mut payload);
        storage_root.encode(&mut payload);
        code_hash.encode(&mut payload);

        // Prepend RLP list header
        let mut buf = Vec::new();
        alloy_rlp::Header {
            list: true,
            payload_length: payload.len(),
        }
        .encode(&mut buf);
        buf.extend_from_slice(&payload);
        buf
    }
}

/// EVM state database
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EvmState {
    pub(crate) accounts: HashMap<Address, EvmAccount>,
}

impl EvmState {
    pub fn new() -> Self {
        Self::default()
    }

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
        if let Some(acc) = self.accounts.get_mut(&address) {
            acc.nonce += 1;
        }
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

    pub fn clone(&self) -> Self {
        Self {
            accounts: self.accounts.clone(),
        }
    }

    /// Get all accounts as a hashmap
    pub fn get_all_accounts(&self) -> &std::collections::HashMap<Address, EvmAccount> {
        &self.accounts
    }

    /// Get all accounts, consuming self
    pub fn into_accounts(self) -> std::collections::HashMap<Address, EvmAccount> {
        self.accounts
    }

    /// Sync EvmState into a revm InMemoryDB
    pub fn sync_to_revm_db(&self, db: &mut InMemoryDB) {
        for (addr, account) in &self.accounts {
            let code = if !account.code.is_empty() {
                Some(Bytecode::new_raw(account.code.clone()))
            } else {
                None
            };
            let info = AccountInfo {
                balance: account.balance,
                nonce: account.nonce,
                code_hash: code.as_ref().map(|c| c.hash_slow()).unwrap_or(revm::primitives::KECCAK_EMPTY),
                account_id: None,
                code,
            };
            db.insert_account_info(*addr, info);

            // Insert storage slots
            for (key, value) in &account.storage {
                let _ = db.insert_account_storage(*addr, *key, *value);
            }
        }
    }

    /// Apply revm state changes back to EvmState
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

    /// Compute the Ethereum state trie root (Merkle Patricia Trie).
    ///
    /// Each account is RLP-encoded as `[nonce, balance, storage_root, code_hash]`
    /// and inserted into the trie at path `keccak256(address)`. The storage root
    /// for each account is itself a Merkle Patricia Trie of its storage slots.
    pub fn compute_state_root(&self) -> B256 {
        let mut hb = HashBuilder::default();
        let mut accounts: Vec<_> = self
            .accounts
            .iter()
            .map(|(addr, acc)| (keccak256(*addr), addr, acc))
            .collect();
        // HashBuilder requires leaves in ascending nibble order.
        accounts.sort_by_key(|(hash, _, _)| *hash);

        for (hash, _address, account) in accounts {
            let storage_root = account.storage_root();
            let account_rlp = account.rlp_encode(storage_root);
            let path = Nibbles::unpack(hash);
            hb.add_leaf(path, &account_rlp);
        }

        hb.root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_evm_account_creation() {
        let mut state = EvmState::new();
        let addr = test_addr(1);
        state.create_account(addr);
        assert!(state.get_account(&addr).is_some());
        assert_eq!(state.get_nonce(&addr), 0);
        assert_eq!(state.get_balance(&addr), U256::ZERO);
    }

    #[test]
    fn test_evm_balance_transfer() {
        let mut state = EvmState::new();
        state.set_balance(test_addr(1), U256::from(1000));
        state.set_balance(test_addr(2), U256::ZERO);

        let bal1 = state.get_balance(&test_addr(1));
        assert_eq!(bal1, U256::from(1000));
    }

    #[test]
    fn test_evm_nonce_increment() {
        let mut state = EvmState::new();
        let addr = test_addr(1);
        state.create_account(addr);
        state.increment_nonce(addr);
        state.increment_nonce(addr);
        assert_eq!(state.get_nonce(&addr), 2);
    }

    #[test]
    fn test_evm_storage() {
        let mut state = EvmState::new();
        let addr = test_addr(1);
        state.set_storage(addr, U256::from(1), U256::from(42));
        assert_eq!(state.get_storage(&addr, U256::from(1)), U256::from(42));
        assert_eq!(state.get_storage(&addr, U256::from(2)), U256::ZERO);
    }
}

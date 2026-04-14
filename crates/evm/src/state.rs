//! EVM state management (accounts, contracts, storage).

use call_primitives::Address;
use alloy_primitives::{U256, Bytes};
use revm::{
    database::InMemoryDB,
    state::AccountInfo,
    bytecode::Bytecode,
};
use std::collections::HashMap;

/// EVM account info
#[derive(Debug, Clone)]
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

/// EVM state database
#[derive(Debug, Default)]
pub struct EvmState {
    accounts: HashMap<Address, EvmAccount>,
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

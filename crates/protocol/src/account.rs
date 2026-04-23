//! T1.2 — Account State (per spec §3.3)
//!
//! Protocol-level account state: balances, allowances, and nonces.

use call_primitives::{Address, AssetId, Balance};
use crate::{ProtocolError, ProtocolResult};
use std::collections::HashMap;

/// Protocol balances: asset_id → address → balance
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountBalances {
    balances: HashMap<(AssetId, Address), Balance>,
}

/// Allowances: (asset_id, owner, spender) → amount
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Allowances {
    allowances: HashMap<(AssetId, Address, Address), Balance>,
}

/// Per-address protocol nonces for replay protection.
/// Nonce is incremented when a transaction is included in a block,
/// regardless of execution result (same as Ethereum).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountNonces {
    nonces: HashMap<Address, u64>,
}

/// Combined protocol account state (balances + allowances + nonces)
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountState {
    pub balances: AccountBalances,
    pub allowances: Allowances,
    pub nonces: AccountNonces,
}

// ── Balance operations ────────────────────────────────────────────────

impl AccountBalances {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.balances.get(&(asset_id, *address)).copied().unwrap_or(0)
    }

    pub fn set_balance(
        &mut self,
        asset_id: AssetId,
        address: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances.insert((asset_id, address), amount);
        Ok(())
    }

    pub fn transfer_balance(
        &mut self,
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let from_bal = self.get_balance(asset_id, &from);
        if from_bal < amount {
            return Err(ProtocolError::InsufficientBalance);
        }
        let to_bal = self.get_balance(asset_id, &to);
        let new_to_bal = to_bal
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        self.balances
            .insert((asset_id, from), from_bal - amount);
        self.balances
            .insert((asset_id, to), new_to_bal);
        Ok(())
    }

    pub fn mint_balance(
        &mut self,
        asset_id: AssetId,
        _issuer: &Address,
        to: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        // In practice, issuer is validated at the instruction level
        let current = self.get_balance(asset_id, &to);
        let new = current
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        self.balances.insert((asset_id, to), new);
        Ok(())
    }

    pub fn burn_balance(
        &mut self,
        asset_id: AssetId,
        from: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let current = self.get_balance(asset_id, &from);
        if current < amount {
            return Err(ProtocolError::InsufficientBalance);
        }
        self.balances
            .insert((asset_id, from), current - amount);
        Ok(())
    }

    pub fn deduct_balance(
        &mut self,
        asset_id: AssetId,
        address: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let current = self.get_balance(asset_id, &address);
        if current < amount {
            return Err(ProtocolError::InsufficientBalance);
        }
        self.balances
            .insert((asset_id, address), current - amount);
        Ok(())
    }

    pub fn credit_balance(
        &mut self,
        asset_id: AssetId,
        address: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let current = self.get_balance(asset_id, &address);
        let new = current
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        self.balances.insert((asset_id, address), new);
        Ok(())
    }

    /// Iterate over all balance entries
    pub fn iter(&self) -> impl Iterator<Item = (&(AssetId, Address), &Balance)> {
        self.balances.iter()
    }

    /// Get a reference to the underlying balances HashMap
    pub fn balances_map(&self) -> &HashMap<(AssetId, Address), Balance> {
        &self.balances
    }
}

// ── Allowance operations ──────────────────────────────────────────────

impl Allowances {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_allowance(
        &self,
        asset_id: AssetId,
        owner: &Address,
        spender: &Address,
    ) -> Balance {
        self.allowances
            .get(&(asset_id, *owner, *spender))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_allowance(
        &mut self,
        asset_id: AssetId,
        owner: Address,
        spender: Address,
        amount: Balance,
    ) {
        self.allowances
            .insert((asset_id, owner, spender), amount);
    }

    pub fn spend_allowance(
        &mut self,
        asset_id: AssetId,
        owner: Address,
        spender: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let current = self.get_allowance(asset_id, &owner, &spender);
        if current < amount {
            return Err(ProtocolError::BalanceError(
                "insufficient allowance".into(),
            ));
        }
        self.allowances
            .insert((asset_id, owner, spender), current - amount);
        Ok(())
    }

    /// Get a reference to the underlying allowances HashMap
    pub fn allowances_map(&self) -> &HashMap<(AssetId, Address, Address), Balance> {
        &self.allowances
    }
}

// ── Nonce operations ──────────────────────────────────────────────────

impl AccountNonces {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.nonces.get(address).copied().unwrap_or(0)
    }

    pub fn set_nonce(&mut self, address: Address, nonce: u64) {
        self.nonces.insert(address, nonce);
    }

    /// Increment nonce for the given address.
    /// Returns the *new* nonce (post-increment, same as Ethereum's nonce usage).
    pub fn increment_nonce(&mut self, address: Address) -> u64 {
        let next = self.get_nonce(&address).saturating_add(1);
        self.nonces.insert(address, next);
        next
    }

    /// Validate that the provided nonce matches the expected next nonce.
    pub fn validate_nonce(&self, address: &Address, nonce: u64) -> ProtocolResult<()> {
        let expected = self.get_nonce(address);
        if nonce != expected {
            return Err(ProtocolError::NonceError(format!(
                "expected nonce {expected}, got {nonce}"
            )));
        }
        Ok(())
    }
}

// ── AccountState ──────────────────────────────────────────────────────

impl AccountState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.balances.get_balance(asset_id, address)
    }

    pub fn transfer(
        &mut self,
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances
            .transfer_balance(asset_id, from, to, amount)
    }

    pub fn mint(
        &mut self,
        asset_id: AssetId,
        issuer: &Address,
        to: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances
            .mint_balance(asset_id, issuer, to, amount)
    }

    pub fn burn(
        &mut self,
        asset_id: AssetId,
        from: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances.burn_balance(asset_id, from, amount)
    }

    pub fn deduct_balance(
        &mut self,
        asset_id: AssetId,
        address: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances.deduct_balance(asset_id, address, amount)
    }

    pub fn credit_balance(
        &mut self,
        asset_id: AssetId,
        address: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        self.balances.credit_balance(asset_id, address, amount)
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.nonces.get_nonce(address)
    }

    pub fn increment_nonce(&mut self, address: Address) -> u64 {
        self.nonces.increment_nonce(address)
    }

    pub fn validate_nonce(&self, address: &Address, nonce: u64) -> ProtocolResult<()> {
        self.nonces.validate_nonce(address, nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_transfer_sufficient_balance() {
        let mut state = AccountState::new();
        state
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        state.transfer(1, test_addr(1), test_addr(2), 500).unwrap();
        assert_eq!(state.get_balance(1, &test_addr(1)), 500);
        assert_eq!(state.get_balance(1, &test_addr(2)), 500);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let mut state = AccountState::new();
        state
            .balances
            .set_balance(1, test_addr(1), 100)
            .unwrap();
        assert!(state.transfer(1, test_addr(1), test_addr(2), 200).is_err());
    }

    #[test]
    fn test_mint_by_non_issuer() {
        let mut state = AccountState::new();
        let non_issuer = test_addr(99);
        state
            .mint(1, &non_issuer, test_addr(1), 500)
            .unwrap();
        assert_eq!(state.get_balance(1, &test_addr(1)), 500);
    }

    #[test]
    fn test_burn_by_non_issuer() {
        let mut state = AccountState::new();
        state
            .balances
            .set_balance(1, test_addr(1), 500)
            .unwrap();
        state.burn(1, test_addr(1), 200).unwrap();
        assert_eq!(state.get_balance(1, &test_addr(1)), 300);
    }

    #[test]
    fn test_allowance_set_and_spend() {
        let mut allowances = Allowances::new();
        let owner = test_addr(1);
        let spender = test_addr(2);
        allowances.set_allowance(1, owner, spender, 100);
        assert_eq!(allowances.get_allowance(1, &owner, &spender), 100);
        allowances
            .spend_allowance(1, owner, spender, 60)
            .unwrap();
        assert_eq!(allowances.get_allowance(1, &owner, &spender), 40);
    }

    #[test]
    fn test_allowance_insufficient() {
        let mut allowances = Allowances::new();
        allowances.set_allowance(1, test_addr(1), test_addr(2), 50);
        assert!(allowances
            .spend_allowance(1, test_addr(1), test_addr(2), 100)
            .is_err());
    }

    #[test]
    fn test_balance_overflow_protection() {
        let mut state = AccountState::new();
        state
            .balances
            .set_balance(1, test_addr(1), Balance::MAX - 100)
            .unwrap();
        assert!(state
            .balances
            .credit_balance(1, test_addr(1), 200)
            .is_err());
    }

    // ── Nonce tests ───────────────────────────────────────────────────

    #[test]
    fn test_nonce_initial_value() {
        let nonces = AccountNonces::new();
        assert_eq!(nonces.get_nonce(&test_addr(1)), 0);
    }

    #[test]
    fn test_nonce_increment() {
        let mut nonces = AccountNonces::new();
        assert_eq!(nonces.increment_nonce(test_addr(1)), 1);
        assert_eq!(nonces.get_nonce(&test_addr(1)), 1);
        assert_eq!(nonces.increment_nonce(test_addr(1)), 2);
        assert_eq!(nonces.get_nonce(&test_addr(1)), 2);
    }

    #[test]
    fn test_nonce_validate_correct() {
        let nonces = AccountNonces::new();
        assert!(nonces.validate_nonce(&test_addr(1), 0).is_ok());
        let mut nonces = AccountNonces::new();
        nonces.increment_nonce(test_addr(1));
        assert!(nonces.validate_nonce(&test_addr(1), 1).is_ok());
    }

    #[test]
    fn test_nonce_validate_wrong() {
        let nonces = AccountNonces::new();
        assert!(nonces.validate_nonce(&test_addr(1), 1).is_err());
        assert!(nonces.validate_nonce(&test_addr(1), 5).is_err());
    }

    #[test]
    fn test_nonce_independent_per_address() {
        let mut nonces = AccountNonces::new();
        nonces.increment_nonce(test_addr(1));
        nonces.increment_nonce(test_addr(2));
        nonces.increment_nonce(test_addr(2));
        assert_eq!(nonces.get_nonce(&test_addr(1)), 1);
        assert_eq!(nonces.get_nonce(&test_addr(2)), 2);
    }

    #[test]
    fn test_account_state_nonce_methods() {
        let mut state = AccountState::new();
        assert_eq!(state.get_nonce(&test_addr(1)), 0);
        state.increment_nonce(test_addr(1));
        assert_eq!(state.get_nonce(&test_addr(1)), 1);
        assert!(state.validate_nonce(&test_addr(1), 1).is_ok());
        assert!(state.validate_nonce(&test_addr(1), 0).is_err());
    }
}

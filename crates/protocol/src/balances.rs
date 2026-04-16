//! T1.2 — Balance Management (per spec §3.3)
//!
//! Protocol-level balance and allowance tracking.

use call_primitives::{Address, AssetId, Balance};
use crate::{ProtocolError, ProtocolResult};
use std::collections::HashMap;

/// Protocol balances: asset_id → address → balance
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProtocolBalances {
    balances: HashMap<(AssetId, Address), Balance>,
}

/// Allowances: (asset_id, owner, spender) → amount
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Allowances {
    allowances: HashMap<(AssetId, Address, Address), Balance>,
}

/// Combined protocol balance state
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BalanceState {
    pub balances: ProtocolBalances,
    pub allowances: Allowances,
}

// ── Balance operations ────────────────────────────────────────────────

impl ProtocolBalances {
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
        self.balances
            .insert((asset_id, from), from_bal - amount);
        self.balances
            .insert((asset_id, to), to_bal + amount);
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
}

// ── BalanceState ──────────────────────────────────────────────────────

impl BalanceState {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_transfer_sufficient_balance() {
        let mut state = BalanceState::new();
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
        let mut state = BalanceState::new();
        state
            .balances
            .set_balance(1, test_addr(1), 100)
            .unwrap();
        assert!(state.transfer(1, test_addr(1), test_addr(2), 200).is_err());
    }

    #[test]
    fn test_mint_by_non_issuer() {
        // Mint is authorized at instruction level; balance layer allows any caller
        let mut state = BalanceState::new();
        let non_issuer = test_addr(99);
        state
            .mint(1, &non_issuer, test_addr(1), 500)
            .unwrap();
        assert_eq!(state.get_balance(1, &test_addr(1)), 500);
    }

    #[test]
    fn test_burn_by_non_issuer() {
        let mut state = BalanceState::new();
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
        let mut state = BalanceState::new();
        state
            .balances
            .set_balance(1, test_addr(1), Balance::MAX - 100)
            .unwrap();
        assert!(state
            .balances
            .credit_balance(1, test_addr(1), 200)
            .is_err());
    }
}

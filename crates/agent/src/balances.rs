//! Agent balance management (per spec §6.4)
//!
//! AgentBalances: HashMap<(Address, u64, AssetId), u128>
//! AgentNonces: HashMap<(Address, u64), u64>

use call_primitives::{Address, AssetId};
use crate::AgentError;

/// Agent balances: keyed by (owner_address, agent_id, asset_id)
///
/// Per spec §6.4: agents have separate balances from the owner.
/// Owners fund agents via Grant/TopUp, and can Revoke funds.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentBalances {
    /// (owner, agent_id, asset_id) -> balance
    balances: std::collections::HashMap<(Address, u64, AssetId), u128>,
}

impl AgentBalances {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get balance for an agent
    pub fn get_balance(
        &self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
    ) -> u128 {
        self.balances
            .get(&(owner, agent_id, asset_id))
            .copied()
            .unwrap_or(0)
    }

    /// Set balance for an agent
    pub fn set_balance(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
    ) {
        self.balances.insert((owner, agent_id, asset_id), amount);
    }

    /// Deduct from agent balance
    pub fn deduct(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
    ) -> Result<(), AgentError> {
        let key = (owner, agent_id, asset_id);
        let balance = self.balances.get(&key).copied().unwrap_or(0);
        if balance < amount {
            return Err(AgentError::InsufficientAgentBalance(agent_id, amount));
        }
        self.balances.insert(key, balance - amount);
        Ok(())
    }

    /// Credit to agent balance
    pub fn credit(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
    ) -> Result<(), AgentError> {
        let key = (owner, agent_id, asset_id);
        let balance = self.balances.get(&key).copied().unwrap_or(0);
        let new_balance = balance.checked_add(amount)
            .ok_or(AgentError::ExecutionFailed("agent balance overflow".into()))?;
        self.balances.insert(key, new_balance);
        Ok(())
    }

    /// Grant funds to an agent (owner -> agent).
    /// Deducts from the owner's protocol balance and credits the agent.
    pub fn grant_funds(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
        protocol_balances: &mut call_protocol::balances::BalanceState,
    ) -> Result<(), AgentError> {
        protocol_balances.deduct_balance(asset_id, owner, amount)
            .map_err(|_| AgentError::ExecutionFailed("insufficient owner balance for grant".into()))?;
        self.credit(owner, agent_id, asset_id, amount)?;
        Ok(())
    }

    /// Top up agent balance (same as grant, semantic difference)
    pub fn top_up(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
        protocol_balances: &mut call_protocol::balances::BalanceState,
    ) -> Result<(), AgentError> {
        self.grant_funds(owner, agent_id, asset_id, amount, protocol_balances)
    }

    /// Revoke funds from agent (agent -> owner)
    pub fn revoke_funds(
        &mut self,
        owner: Address,
        agent_id: u64,
        asset_id: AssetId,
    ) -> u128 {
        let key = (owner, agent_id, asset_id);
        let balance = self.balances.get(&key).copied().unwrap_or(0);
        self.balances.remove(&key);
        balance
    }

    /// Get total balance across all assets for an agent
    pub fn get_total_balance(&self, owner: Address, agent_id: u64) -> u128 {
        self.balances
            .iter()
            .filter(|((o, a, _), _)| *o == owner && *a == agent_id)
            .map(|(_, &balance)| balance)
            .sum()
    }

    /// Get a reference to the underlying balances HashMap
    pub fn balances_map(&self) -> &std::collections::HashMap<(Address, u64, AssetId), u128> {
        &self.balances
    }
}

/// Agent nonces: keyed by (owner, agent_id)
///
/// Per spec §6.6: agent transactions have their own nonce sequence
/// to prevent replay attacks.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentNonces {
    /// (owner, agent_id) -> nonce
    nonces: std::collections::HashMap<(Address, u64), u64>,
}

impl AgentNonces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get current nonce for an agent
    pub fn get_nonce(&self, owner: Address, agent_id: u64) -> u64 {
        self.nonces.get(&(owner, agent_id)).copied().unwrap_or(0)
    }

    /// Check and increment nonce (returns error if stale/duplicate)
    pub fn check_and_increment(
        &mut self,
        owner: Address,
        agent_id: u64,
        expected_nonce: u64,
    ) -> Result<(), AgentError> {
        let key = (owner, agent_id);
        let current = self.nonces.get(&key).copied().unwrap_or(0);

        if expected_nonce != current {
            return Err(AgentError::AgentNonceError(format!(
                "expected nonce {}, got {}",
                current, expected_nonce
            )));
        }

        self.nonces.insert(key, current + 1);
        Ok(())
    }

    /// Force set nonce (for recovery or admin operations)
    pub fn set_nonce(&mut self, owner: Address, agent_id: u64, nonce: u64) {
        self.nonces.insert((owner, agent_id), nonce);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;

    #[test]
    fn test_agent_grant_funding() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 10_000).unwrap();
        balances.grant_funds(owner, 0, 1, 5000, &mut protocol_balances).unwrap();
        assert_eq!(balances.get_balance(owner, 0, 1), 5000);
        assert_eq!(protocol_balances.get_balance(1, &owner), 5000);
    }

    #[test]
    fn test_agent_grant_funding_insufficient_owner_balance() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 100).unwrap();
        assert!(balances.grant_funds(owner, 0, 1, 5000, &mut protocol_balances).is_err());
    }

    #[test]
    fn test_agent_top_up() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 10_000).unwrap();
        balances.grant_funds(owner, 0, 1, 1000, &mut protocol_balances).unwrap();
        balances.top_up(owner, 0, 1, 2000, &mut protocol_balances).unwrap();
        assert_eq!(balances.get_balance(owner, 0, 1), 3000);
        assert_eq!(protocol_balances.get_balance(1, &owner), 7000);
    }

    #[test]
    fn test_agent_revoke_by_owner() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 10_000).unwrap();
        balances.grant_funds(owner, 0, 1, 5000, &mut protocol_balances).unwrap();
        let revoked = balances.revoke_funds(owner, 0, 1);
        assert_eq!(revoked, 5000);
        assert_eq!(balances.get_balance(owner, 0, 1), 0);
    }

    #[test]
    fn test_agent_deduct_insufficient() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 10_000).unwrap();
        balances.grant_funds(owner, 0, 1, 100, &mut protocol_balances).unwrap();
        assert!(balances.deduct(owner, 0, 1, 200).is_err());
        assert!(balances.deduct(owner, 0, 1, 100).is_ok());
        assert_eq!(balances.get_balance(owner, 0, 1), 0);
    }

    #[test]
    fn test_agent_credit_overflow() {
        let mut balances = AgentBalances::new();
        let owner = test_addr(1);
        balances.set_balance(owner, 0, 1, u128::MAX);
        let result = balances.credit(owner, 0, 1, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_nonce_tracking() {
        let mut nonces = AgentNonces::new();
        let owner = test_addr(1);

        assert_eq!(nonces.get_nonce(owner, 0), 0);
        assert!(nonces.check_and_increment(owner, 0, 0).is_ok());
        assert_eq!(nonces.get_nonce(owner, 0), 1);
        assert!(nonces.check_and_increment(owner, 0, 1).is_ok());
        assert_eq!(nonces.get_nonce(owner, 0), 2);

        // Stale nonce
        assert!(nonces.check_and_increment(owner, 0, 0).is_err());
        // Duplicate nonce
        assert!(nonces.check_and_increment(owner, 0, 1).is_err());
    }

    #[test]
    fn test_agent_total_balance() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 1000).unwrap();
        protocol_balances.balances.set_balance(2, owner, 2000).unwrap();
        protocol_balances.balances.set_balance(3, owner, 3000).unwrap();
        balances.grant_funds(owner, 0, 1, 1000, &mut protocol_balances).unwrap();
        balances.grant_funds(owner, 0, 2, 2000, &mut protocol_balances).unwrap();
        balances.grant_funds(owner, 0, 3, 3000, &mut protocol_balances).unwrap();

        assert_eq!(balances.get_total_balance(owner, 0), 6000);
    }

    #[test]
    fn test_agent_balances_isolated_by_owner() {
        let mut balances = AgentBalances::new();
        let mut protocol_balances = call_protocol::balances::BalanceState::new();
        protocol_balances.balances.set_balance(1, test_addr(1), 1000).unwrap();
        protocol_balances.balances.set_balance(1, test_addr(2), 2000).unwrap();
        balances.grant_funds(test_addr(1), 0, 1, 1000, &mut protocol_balances).unwrap();
        balances.grant_funds(test_addr(2), 0, 1, 2000, &mut protocol_balances).unwrap();

        assert_eq!(balances.get_balance(test_addr(1), 0, 1), 1000);
        assert_eq!(balances.get_balance(test_addr(2), 0, 1), 2000);
    }
}

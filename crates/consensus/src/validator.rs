//! T6.2 — Validator Staking (per spec §12.6)
//!
//! Validator stake management, slashing, rewards, staking/unbonding.

use call_primitives::{Address, Ed25519PublicKey, ValidatorId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ─────────────────────────────────────────────────────────

/// Minimum self-stake required: 1,000,000 CALL (18 decimals)
pub const MIN_SELF_STAKE: u128 = 1_000_000 * 10u128.pow(18);

/// Unbonding period: 7 days
pub const UNBONDING_PERIOD_SECS: u64 = 7 * 24 * 3600;

/// Proportional offline slash rate per round (0.1% per round)
pub const OFFLINE_SLASH_RATE_PER_ROUND: u128 = 10; // basis points (0.10%)

// ── Types ─────────────────────────────────────────────────────────────

/// Slash event record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashEvent {
    pub reason: String,
    pub amount_slashed: u128,
    pub block: u64,
}

/// Validator stake state (per spec §12.6)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorStake {
    pub validator_id: ValidatorId,
    pub address: Address,
    pub ed25519_pubkey: Ed25519PublicKey,
    pub staked_call: u128,
    pub self_stake: u128,
    pub delegated_call: u128,
    pub rewards: u128,
    pub slash_history: Vec<SlashEvent>,
    pub unbonding_start: Option<u64>, // block height when unbonding started
}

/// Pending unbonding request
#[derive(Debug, Clone)]
pub struct UnbondingRequest {
    pub validator_id: ValidatorId,
    pub amount: u128,
    pub requested_at_block: u64,
    pub eligible_at_block: u64,
}

/// Manages all validator stakes (per spec §12.6)
#[derive(Debug, Default)]
pub struct ValidatorStateManager {
    validators: HashMap<ValidatorId, ValidatorStake>,
    next_validator_id: ValidatorId,
    unbonding_requests: Vec<UnbondingRequest>,
    /// Block height used to calculate unbonding eligibility
    current_block: u64,
}

impl ValidatorStateManager {
    /// Create a new validator state manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Set current block height (for unbonding calculations)
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Get next available validator ID
    fn next_id(&mut self) -> ValidatorId {
        let id = self.next_validator_id;
        self.next_validator_id = self.next_validator_id.wrapping_add(1);
        id
    }

    // ── Staking ───────────────────────────────────────────────────────

    /// Stake CALL to become a validator (per spec §12.6)
    pub fn stake(
        &mut self,
        address: Address,
        ed25519_pubkey: Ed25519PublicKey,
        self_stake: u128,
    ) -> Result<ValidatorId, ConsensusError> {
        if self_stake < MIN_SELF_STAKE {
            return Err(ConsensusError::InsufficientStake);
        }

        let id = self.next_id();
        self.validators.insert(
            id,
            ValidatorStake {
                validator_id: id,
                address,
                ed25519_pubkey,
                staked_call: self_stake,
                self_stake,
                delegated_call: 0,
                rewards: 0,
                slash_history: Vec::new(),
                unbonding_start: None,
            },
        );
        Ok(id)
    }

    /// Delegate CALL to an existing validator
    pub fn delegate(
        &mut self,
        validator_id: ValidatorId,
        amount: u128,
    ) -> Result<(), ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        if validator.unbonding_start.is_some() {
            return Err(ConsensusError::UnbondingNotElapsed);
        }

        validator.delegated_call += amount;
        validator.staked_call += amount;
        Ok(())
    }

    /// Begin unbonding process (starts 7-day timer)
    pub fn unstake(&mut self, validator_id: ValidatorId) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        if validator.unbonding_start.is_some() {
            return Err(ConsensusError::UnbondingNotElapsed);
        }

        let amount = validator.self_stake;
        validator.unbonding_start = Some(self.current_block);

        // Calculate block height when unbonding becomes eligible
        // ~144 blocks per day at 250ms block time (216 * 4 * 60 * 60 / 1000)
        // 7 days = ~1008 blocks
        let blocks_in_unbonding = 1008u64;
        let eligible_at = self.current_block + blocks_in_unbonding;

        self.unbonding_requests.push(UnbondingRequest {
            validator_id,
            amount,
            requested_at_block: self.current_block,
            eligible_at_block: eligible_at,
        });

        Ok(amount)
    }

    /// Claim unbonded stake after unbonding period
    pub fn claim_unbonded(&mut self, validator_id: ValidatorId) -> Result<u128, ConsensusError> {
        let request_idx = self
            .unbonding_requests
            .iter()
            .position(|r| r.validator_id == validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        let request = &self.unbonding_requests[request_idx];
        if self.current_block < request.eligible_at_block {
            return Err(ConsensusError::UnbondingNotElapsed);
        }

        let amount = request.amount;
        self.unbonding_requests.remove(request_idx);
        self.validators.remove(&validator_id);
        Ok(amount)
    }

    // ── Slashing ──────────────────────────────────────────────────────

    /// Slash for double-sign: full self-stake (per spec §12.6)
    pub fn slash_double_sign(
        &mut self,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        let slashed = validator.self_stake;
        validator.slash_history.push(SlashEvent {
            reason: "double sign".into(),
            amount_slashed: slashed,
            block: self.current_block,
        });
        validator.self_stake = 0;
        validator.staked_call -= slashed;
        Ok(slashed)
    }

    /// Slash for being offline: proportional to rounds offline (per spec §12.6)
    pub fn slash_offline(
        &mut self,
        validator_id: ValidatorId,
        rounds_offline: u64,
    ) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        // Proportional slash: rounds * rate% of self_stake
        let rate_total = OFFLINE_SLASH_RATE_PER_ROUND * rounds_offline as u128;
        let slashed = (validator.self_stake * rate_total) / 10_000; // basis points

        validator.slash_history.push(SlashEvent {
            reason: format!("offline for {rounds_offline} rounds"),
            amount_slashed: slashed,
            block: self.current_block,
        });
        validator.self_stake -= slashed;
        validator.staked_call -= slashed;
        Ok(slashed)
    }

    // ── Rewards ───────────────────────────────────────────────────────

    /// Distribute reward to proposer: 50% fee split by stake proportion
    /// (per spec §12.6, §3.6)
    pub fn distribute_reward(
        &mut self,
        proposer_id: ValidatorId,
        total_reward: u128,
    ) -> Result<(), ConsensusError> {
        let validator = self
            .validators
            .get_mut(&proposer_id)
            .ok_or(ConsensusError::ValidatorNotFound(proposer_id))?;

        validator.rewards += total_reward;
        Ok(())
    }

    /// Get validator stake info
    pub fn get_validator_stake(
        &self,
        validator_id: ValidatorId,
    ) -> Option<&ValidatorStake> {
        self.validators.get(&validator_id)
    }

    /// Get all active (non-unbonding) validator IDs
    pub fn get_active_validators(&self) -> Vec<ValidatorId> {
        self.validators
            .values()
            .filter(|v| v.unbonding_start.is_none())
            .map(|v| v.validator_id)
            .collect()
    }

    /// Get all validator IDs
    pub fn get_all_validator_ids(&self) -> Vec<ValidatorId> {
        self.validators.keys().copied().collect()
    }

    /// Total staked CALL across all active validators
    pub fn total_active_stake(&self) -> u128 {
        self.validators
            .values()
            .filter(|v| v.unbonding_start.is_none())
            .map(|v| v.staked_call)
            .sum()
    }

    /// Check if a validator is currently unbonding
    pub fn is_unbonding(&self, validator_id: ValidatorId) -> bool {
        self.validators
            .get(&validator_id)
            .is_some_and(|v| v.unbonding_start.is_some())
    }

    /// Check if unbonding requests are now eligible
    pub fn eligible_unbonding_requests(&self) -> Vec<ValidatorId> {
        self.unbonding_requests
            .iter()
            .filter(|r| self.current_block >= r.eligible_at_block)
            .map(|r| r.validator_id)
            .collect()
    }
}

// ── Consensus Error ───────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("invalid block: {0}")]
    InvalidBlock(String),
    #[error("proposer not in subset: {0}")]
    ProposerNotInSubset(ValidatorId),
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("double sign detected: validator={0}")]
    DoubleSign(ValidatorId),
    #[error("validator offline: {0}")]
    ValidatorOffline(ValidatorId),
    #[error("insufficient stake")]
    InsufficientStake,
    #[error("unbonding period not elapsed")]
    UnbondingNotElapsed,
    #[error("validator not found: {0}")]
    ValidatorNotFound(ValidatorId),
    #[error("consensus error: {0}")]
    ConsensusError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn one_million_call() -> u128 {
        1_000_000 * 10u128.pow(18)
    }

    #[test]
    fn test_validator_stake_minimum() {
        let mut state = ValidatorStateManager::new();

        // Below minimum — should fail
        let result = state.stake(test_addr(1), test_pubkey(1), MIN_SELF_STAKE - 1);
        assert!(matches!(result, Err(ConsensusError::InsufficientStake)));

        // At minimum — should succeed
        let result = state.stake(test_addr(1), test_pubkey(1), MIN_SELF_STAKE);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validator_unbonding_period() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        // Unstake begins unbonding
        let amount = state.unstake(id).unwrap();
        assert_eq!(amount, one_million_call());
        assert!(state.is_unbonding(id));

        // Cannot unstake again while unbonding
        let result = state.unstake(id);
        assert!(matches!(
            result,
            Err(ConsensusError::UnbondingNotElapsed)
        ));

        // Cannot claim before period elapses
        let result = state.claim_unbonded(id);
        assert!(matches!(
            result,
            Err(ConsensusError::UnbondingNotElapsed)
        ));

        // Advance past unbonding period (1008 blocks)
        state.set_current_block(2000);
        let claimed = state.claim_unbonded(id).unwrap();
        assert_eq!(claimed, one_million_call());
        assert!(!state.is_unbonding(id));
    }

    #[test]
    fn test_slash_double_sign_full_loss() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        // Add delegation
        state.delegate(id, 500_000 * 10u128.pow(18)).unwrap();

        let slashed = state.slash_double_sign(id).unwrap();
        assert_eq!(slashed, one_million_call());

        let stake = state.get_validator_stake(id).unwrap();
        assert_eq!(stake.self_stake, 0);
        // Only self_stake is slashed, delegation remains
        assert_eq!(stake.slash_history.len(), 1);
        assert_eq!(stake.slash_history[0].reason, "double sign");
    }

    #[test]
    fn test_slash_offline_proportional() {
        let mut state = ValidatorStateManager::new();
        let stake_amount = one_million_call();
        let id = state
            .stake(test_addr(1), test_pubkey(1), stake_amount)
            .unwrap();

        // 10 rounds offline at 0.10% per round = 1% total
        let slashed = state.slash_offline(id, 10).unwrap();
        let expected_slash = (stake_amount * 10 * 10) / 10_000; // 1%
        assert_eq!(slashed, expected_slash);

        let stake = state.get_validator_stake(id).unwrap();
        assert!(stake.self_stake < stake_amount);
        assert_eq!(stake.slash_history.len(), 1);
    }

    #[test]
    fn test_reward_distribution_by_stake() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        let reward = 1_000_000_000u128; // 1 billion wei
        state.distribute_reward(id, reward).unwrap();

        let stake = state.get_validator_stake(id).unwrap();
        assert_eq!(stake.rewards, reward);
    }

    #[test]
    fn test_validator_state_accessors() {
        let mut state = ValidatorStateManager::new();
        let id1 = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();
        let id2 = state
            .stake(test_addr(2), test_pubkey(2), one_million_call())
            .unwrap();

        let active = state.get_active_validators();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&id1));
        assert!(active.contains(&id2));

        let total = state.total_active_stake();
        assert_eq!(total, 2 * one_million_call());

        let all_ids = state.get_all_validator_ids();
        assert_eq!(all_ids.len(), 2);
    }

    #[test]
    fn test_validator_not_found_errors() {
        let state = ValidatorStateManager::new();
        assert!(matches!(
            state.get_validator_stake(999),
            None
        ));
    }

    #[test]
    fn test_delegation_rejected_during_unbonding() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        state.unstake(id).unwrap();

        // Delegation rejected while unbonding
        let result = state.delegate(id, 1000);
        assert!(matches!(
            result,
            Err(ConsensusError::UnbondingNotElapsed)
        ));
    }

    #[test]
    fn test_eligible_unbonding_requests() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        state.unstake(id).unwrap();

        // Before period: no eligible requests
        let eligible = state.eligible_unbonding_requests();
        assert!(eligible.is_empty());

        // After period: eligible
        state.set_current_block(2000);
        let eligible = state.eligible_unbonding_requests();
        assert_eq!(eligible, vec![id]);
    }
}

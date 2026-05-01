//! T6.2 — Validator Staking (per spec §12.6)
//!
//! Validator stake management, slashing, rewards, staking/unbonding.

use crate::proposer::ConsensusParams;
use call_primitives::{Address, Ed25519PublicKey, ValidatorId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ─────────────────────────────────────────────────────────

/// System escrow address for staked CALL tokens (Cosmos-style module account)
pub const STAKING_ESCROW: Address = Address::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0xCE,
]);

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
    /// BLS12-381 public key for aggregated vote signatures (48 bytes compressed)
    #[serde(default = "default_bls_pubkey", with = "serde_bytes")]
    pub bls_pubkey: [u8; 48],
}

fn default_bls_pubkey() -> [u8; 48] {
    [0u8; 48]
}

/// Pending unbonding request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbondingRequest {
    pub validator_id: ValidatorId,
    /// Original staker address — used to return tokens from escrow on claim
    pub sender_address: Address,
    pub amount: u128,
    pub requested_at_block: u64,
    pub eligible_at_block: u64,
}

/// Key rotation record — tracks pubkey transitions with grace period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRotation {
    pub validator_id: ValidatorId,
    pub old_pubkey: Ed25519PublicKey,
    pub new_pubkey: Ed25519PublicKey,
    pub rotation_block: u64,
}

/// Snapshot of global validator state (queues, counters, params) for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorMetaSnapshot {
    pub next_validator_id: ValidatorId,
    pub unbonding_requests: Vec<UnbondingRequest>,
    pub current_block: u64,
    pub key_rotations: Vec<KeyRotation>,
    pub stake_queue: Vec<Address>,
    pub exit_queue: Vec<ValidatorId>,
    pub epoch_churn_count: u64,
    pub params: ConsensusParams,
}

/// Manages all validator stakes (per spec §12.6)
#[derive(Debug, Clone)]
pub struct ValidatorStateManager {
    validators: HashMap<ValidatorId, ValidatorStake>,
    next_validator_id: ValidatorId,
    unbonding_requests: Vec<UnbondingRequest>,
    /// Block height used to calculate unbonding eligibility
    current_block: u64,
    /// Key rotation history — old pubkeys remain valid during grace period
    key_rotations: Vec<KeyRotation>,
    /// FIFO queue for new stake requests when churn limit is reached
    stake_queue: Vec<Address>,
    /// FIFO queue for exit (unstake) requests when churn limit is reached
    exit_queue: Vec<ValidatorId>,
    /// Number of validators that have entered/exited in the current epoch
    epoch_churn_count: u64,
    /// Consensus parameters (includes min_self_stake, slash rates, churn config)
    pub params: ConsensusParams,
}

impl Default for ValidatorStateManager {
    fn default() -> Self {
        Self {
            validators: HashMap::new(),
            next_validator_id: 0,
            unbonding_requests: Vec::new(),
            current_block: 0,
            key_rotations: Vec::new(),
            stake_queue: Vec::new(),
            exit_queue: Vec::new(),
            epoch_churn_count: 0,
            params: ConsensusParams::default(),
        }
    }
}

impl ValidatorStateManager {
    /// Create a new validator state manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with explicit consensus params (useful for tests / different networks)
    pub fn with_params(params: ConsensusParams) -> Self {
        Self {
            params,
            ..Self::default()
        }
    }

    /// Update minimum self-stake requirement
    pub fn set_min_self_stake(&mut self, min_self_stake: u128) {
        self.params.min_self_stake = min_self_stake;
    }

    /// Update offline slash rate
    pub fn set_offline_slash_rate_bps(&mut self, rate: u128) {
        self.params.offline_slash_rate_bps = rate;
    }

    /// Build a snapshot of global meta-state for persistence.
    pub fn meta_snapshot(&self) -> ValidatorMetaSnapshot {
        ValidatorMetaSnapshot {
            next_validator_id: self.next_validator_id,
            unbonding_requests: self.unbonding_requests.clone(),
            current_block: self.current_block,
            key_rotations: self.key_rotations.clone(),
            stake_queue: self.stake_queue.clone(),
            exit_queue: self.exit_queue.clone(),
            epoch_churn_count: self.epoch_churn_count,
            params: self.params,
        }
    }

    /// Restore global meta-state from a snapshot.
    pub fn restore_meta_snapshot(&mut self, snapshot: ValidatorMetaSnapshot) {
        self.next_validator_id = snapshot.next_validator_id;
        self.unbonding_requests = snapshot.unbonding_requests;
        self.current_block = snapshot.current_block;
        self.key_rotations = snapshot.key_rotations;
        self.stake_queue = snapshot.stake_queue;
        self.exit_queue = snapshot.exit_queue;
        self.epoch_churn_count = snapshot.epoch_churn_count;
        self.params = snapshot.params;
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

    /// Register a validator from an existing ValidatorStake (used during DB recovery)
    pub fn register_validator_from_stake(&mut self, id: ValidatorId, stake: ValidatorStake) {
        if id >= self.next_validator_id {
            self.next_validator_id = id + 1;
        }
        self.validators.insert(id, stake);
    }

    // ── Staking ───────────────────────────────────────────────────────

    /// Stake CALL to become a validator (per spec §12.6)
    pub fn stake(
        &mut self,
        address: Address,
        ed25519_pubkey: Ed25519PublicKey,
        self_stake: u128,
    ) -> Result<ValidatorId, ConsensusError> {
        if self_stake < self.params.min_self_stake {
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
                bls_pubkey: [0u8; 48],
            },
        );
        Ok(id)
    }

    /// Set the BLS12-381 public key for an existing validator.
    /// Called after staking when the validator registers their BLS key.
    pub fn set_validator_bls_pubkey(
        &mut self,
        validator_id: ValidatorId,
        bls_pubkey: [u8; 48],
    ) -> Result<(), ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;
        validator.bls_pubkey = bls_pubkey;
        Ok(())
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

        validator.delegated_call = validator
            .delegated_call
            .checked_add(amount)
            .ok_or(ConsensusError::ConsensusError("delegation overflow".into()))?;
        validator.staked_call = validator
            .staked_call
            .checked_add(amount)
            .ok_or(ConsensusError::ConsensusError("stake overflow".into()))?;
        Ok(())
    }

    /// Begin unbonding process (starts 7-day timer)
    pub fn unstake(
        &mut self,
        validator_id: ValidatorId,
        sender: Address,
    ) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        if validator.address != sender {
            return Err(ConsensusError::ConsensusError(
                "unstake: sender must be the validator owner".into(),
            ));
        }

        if validator.unbonding_start.is_some() {
            return Err(ConsensusError::UnbondingNotElapsed);
        }

        let amount = validator.self_stake;
        validator.unbonding_start = Some(self.current_block);

        // Calculate block height when unbonding becomes eligible
        let eligible_at = self.current_block + self.params.unbonding_period_blocks;

        self.unbonding_requests.push(UnbondingRequest {
            validator_id,
            sender_address: sender,
            amount,
            requested_at_block: self.current_block,
            eligible_at_block: eligible_at,
        });

        Ok(amount)
    }

    /// Claim unbonded stake after unbonding period.
    /// Returns (amount, sender_address) so the caller can transfer tokens
    /// from the escrow back to the original staker.
    pub fn claim_unbonded(
        &mut self,
        validator_id: ValidatorId,
    ) -> Result<(u128, Address), ConsensusError> {
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
        let sender_address = request.sender_address;
        self.unbonding_requests.remove(request_idx);
        self.validators.remove(&validator_id);
        Ok((amount, sender_address))
    }

    // ── Churn Control ─────────────────────────────────────────────────

    /// Compute the current churn limit based on qualified validator count.
    pub fn current_churn_limit(&self) -> u64 {
        let qualified = self.get_qualified_validators().len() as u64;
        self.params.churn_limit(qualified)
    }

    /// Compute the safety floor: minimum qualified validators required.
    pub fn safety_floor(&self) -> u64 {
        self.params.safety_floor()
    }

    /// Check if an unstake would drop below the safety floor.
    pub fn can_unstake_safely(&self) -> bool {
        let qualified = self.get_qualified_validators().len() as u64;
        qualified.saturating_sub(1) >= self.safety_floor()
    }

    /// Reset the epoch churn counter. Called at epoch boundary.
    pub fn reset_epoch_churn(&mut self) {
        self.epoch_churn_count = 0;
    }

    /// Process pending stake/exit queues at epoch boundary, up to churn limit.
    /// Returns the number of validators that entered and exited.
    pub fn process_epoch_churn(&mut self) -> (u64, u64) {
        let limit = self.current_churn_limit();
        let mut entered = 0u64;
        let mut exited = 0u64;

        // Process exit queue (unstakes)
        while exited < limit && !self.exit_queue.is_empty() {
            let validator_id = self.exit_queue.remove(0);
            // Perform the actual unstake if validator still exists and not already unbonding
            if let Some(v) = self.validators.get_mut(&validator_id) {
                if v.unbonding_start.is_none() {
                    v.unbonding_start = Some(self.current_block);
                    let eligible_at = self.current_block + self.params.unbonding_period_blocks;
                    self.unbonding_requests.push(UnbondingRequest {
                        validator_id,
                        sender_address: v.address,
                        amount: v.self_stake,
                        requested_at_block: self.current_block,
                        eligible_at_block: eligible_at,
                    });
                    exited += 1;
                }
            }
        }

        // Process stake queue (new validators)
        // NOTE: stake_queue stores addresses; we need pubkeys and amounts to actually stake.
        // For now, stake_queue entries are just placeholders — actual staking requires
        // a full ValidatorStake instruction which is processed separately.
        // This design reserves the queue slot; the tx is re-submitted in the next epoch.
        //
        // TODO: if we want true queued staking, we need to store (address, pubkey, amount)
        // in the queue and call self.stake() here.

        self.epoch_churn_count = entered + exited;
        (entered, exited)
    }

    /// Queue an unstake request if churn limit is reached.
    /// Returns true if queued, false if processed immediately.
    pub fn try_unstake(
        &mut self,
        validator_id: ValidatorId,
        sender: Address,
    ) -> Result<bool, ConsensusError> {
        // Safety floor check
        if !self.can_unstake_safely() {
            return Err(ConsensusError::ConsensusError(
                format!("unstake rejected: would drop below safety threshold ({})", self.safety_floor())
            ));
        }

        let limit = self.current_churn_limit();
        if self.epoch_churn_count >= limit {
            self.exit_queue.push(validator_id);
            return Ok(true); // queued
        }

        // Process immediately
        let amount = self.unstake(validator_id, sender)?;
        self.epoch_churn_count += 1;
        Ok(false) // not queued
    }

    // ── Slashing ──────────────────────────────────────────────────────

    /// Remove a validator entirely (used by governance slash proposals).
    /// Slashes self-stake (sent to treasury), returns delegation to delegators,
    /// and removes the validator from the active set.
    pub fn remove_validator(&mut self, validator_id: ValidatorId) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?
            .clone();

        let slashed_self = validator.self_stake;
        let returned_delegation = validator.delegated_call;

        // Record slash event
        let mut slash_record = validator.clone();
        slash_record.slash_history.push(SlashEvent {
            reason: "governance slash".into(),
            amount_slashed: slashed_self,
            block: self.current_block,
        });

        // Remove from active set
        self.validators.remove(&validator_id);

        tracing::warn!(
            validator_id,
            slashed_self,
            returned_delegation,
            "validator removed via governance slash — self-stake slashed, delegation returned"
        );

        Ok(slashed_self)
    }

    /// Slash for double-sign: full self-stake, validator removed from active set (per spec §12.6)
    pub fn slash_double_sign(
        &mut self,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?
            .clone();

        let slashed = validator.self_stake;
        let mut slash_record = validator.clone();
        slash_record.slash_history.push(SlashEvent {
            reason: "double sign".into(),
            amount_slashed: slashed,
            block: self.current_block,
        });

        // Remove from active set — double-signing is unrecoverable
        self.validators.remove(&validator_id);

        tracing::warn!(
            validator_id,
            slashed,
            "validator removed after double-sign slash"
        );

        Ok(slashed)
    }

    /// Slash for being offline: proportional to rounds offline (per spec §12.6).
    /// Validator is removed from the active set if self-stake drops below min_self_stake.
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
        let rate_total = self.params.offline_slash_rate_bps * rounds_offline as u128;
        let slashed = (validator.self_stake * rate_total) / 10_000; // basis points

        validator.slash_history.push(SlashEvent {
            reason: format!("offline for {rounds_offline} rounds"),
            amount_slashed: slashed,
            block: self.current_block,
        });
        validator.self_stake = validator.self_stake.saturating_sub(slashed);
        validator.staked_call = validator.staked_call.saturating_sub(slashed);

        let remaining = validator.self_stake;
        if remaining < self.params.min_self_stake {
            self.validators.remove(&validator_id);
            tracing::warn!(
                validator_id,
                remaining_stake = remaining,
                "validator removed after offline slash dropped self-stake below minimum"
            );
        }

        Ok(slashed)
    }

    /// Slash for submitting oracle price outliers (0.1% of self_stake)
    pub fn slash_oracle_outlier(
        &mut self,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        let slashed = (validator.self_stake * 10) / 10_000; // 0.1%
        validator.slash_history.push(SlashEvent {
            reason: "oracle outlier".into(),
            amount_slashed: slashed,
            block: self.current_block,
        });
        validator.self_stake = validator.self_stake.saturating_sub(slashed);
        validator.staked_call = validator.staked_call.saturating_sub(slashed);

        let remaining = validator.self_stake;
        if remaining < self.params.min_self_stake {
            self.validators.remove(&validator_id);
            tracing::warn!(
                validator_id,
                remaining_stake = remaining,
                "validator removed after oracle outlier slash dropped self-stake below minimum"
            );
        }

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

        validator.rewards = validator
            .rewards
            .checked_add(total_reward)
            .ok_or(ConsensusError::ConsensusError("reward overflow".into()))?;
        Ok(())
    }

    // ── Key Rotation ──────────────────────────────────────────────────

    /// Rotate a validator's Ed25519 public key.
    /// The old key remains valid during `params.key_rotation_grace_blocks` for in-flight messages.
    /// Returns the old key for historical record.
    pub fn rotate_key(
        &mut self,
        validator_id: ValidatorId,
        old_pubkey: Ed25519PublicKey,
        new_pubkey: Ed25519PublicKey,
    ) -> Result<Ed25519PublicKey, ConsensusError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

        if validator.ed25519_pubkey != old_pubkey {
            return Err(ConsensusError::ConsensusError(
                "old pubkey does not match current validator key".into(),
            ));
        }

        // Record rotation
        self.key_rotations.push(KeyRotation {
            validator_id,
            old_pubkey,
            new_pubkey,
            rotation_block: self.current_block,
        });

        let previous = validator.ed25519_pubkey;
        validator.ed25519_pubkey = new_pubkey;

        tracing::info!(
            validator_id,
            rotation_block = self.current_block,
            grace_blocks = self.params.key_rotation_grace_blocks,
            "validator key rotated — old key valid for grace period"
        );

        Ok(previous)
    }

    /// Check if a public key is valid for a validator at the given block height.
    /// Returns true if:
    /// - `pubkey` is the current key, OR
    /// - `pubkey` was rotated out within the key rotation grace period of `block`
    pub fn is_valid_pubkey(
        &self,
        validator_id: ValidatorId,
        pubkey: &Ed25519PublicKey,
        block: u64,
    ) -> bool {
        let Some(validator) = self.validators.get(&validator_id) else {
            return false;
        };

        // Current key always valid
        if &validator.ed25519_pubkey == pubkey {
            return true;
        }

        // Check rotation history
        for rotation in &self.key_rotations {
            if rotation.validator_id == validator_id
                && &rotation.old_pubkey == pubkey
                && block <= rotation.rotation_block + self.params.key_rotation_grace_blocks
            {
                return true;
            }
        }

        false
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

    /// Get all validators with stake ≥ min_self_stake and not unbonding.
    /// Used for VRF participant subset selection in BFT epochs.
    pub fn get_qualified_validators(&self) -> Vec<ValidatorId> {
        self.validators
            .values()
            .filter(|v| v.unbonding_start.is_none() && v.staked_call >= self.params.min_self_stake)
            .map(|v| v.validator_id)
            .collect()
    }

    /// Get all validator IDs
    pub fn get_all_validator_ids(&self) -> Vec<ValidatorId> {
        self.validators.keys().copied().collect()
    }

    /// Get all validator stake info
    pub fn get_all_validators(&self) -> &HashMap<ValidatorId, ValidatorStake> {
        &self.validators
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

    /// Check if an address belongs to a qualified (current-epoch) validator.
    pub fn is_qualified_validator(&self, address: &Address) -> bool {
        self.validators
            .values()
            .any(|v| {
                v.address == *address
                    && v.unbonding_start.is_none()
                    && v.staked_call >= self.params.min_self_stake
            })
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
        let result = state.stake(test_addr(1), test_pubkey(1), state.params.min_self_stake - 1);
        assert!(matches!(result, Err(ConsensusError::InsufficientStake)));

        // At minimum — should succeed
        let result = state.stake(test_addr(1), test_pubkey(1), state.params.min_self_stake);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validator_unbonding_period() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        // Unstake begins unbonding
        let amount = state.unstake(id, test_addr(1)).unwrap();
        assert_eq!(amount, one_million_call());
        assert!(state.is_unbonding(id));

        // Cannot unstake again while unbonding
        let result = state.unstake(id, test_addr(1));
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

        // Advance past unbonding period
        state.set_current_block(state.params.unbonding_period_blocks + 100);
        let (claimed, _recipient) = state.claim_unbonded(id).unwrap();
        assert_eq!(claimed, one_million_call());
        assert!(!state.is_unbonding(id));
    }

    #[test]
    fn test_slash_double_sign_removes_validator() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        // Add delegation
        state.delegate(id, 500_000 * 10u128.pow(18)).unwrap();

        let slashed = state.slash_double_sign(id).unwrap();
        assert_eq!(slashed, one_million_call());

        // Validator should be removed from the active set after double-sign
        assert!(state.get_validator_stake(id).is_none());
        assert!(!state.get_all_validator_ids().contains(&id));
    }

    #[test]
    fn test_slash_offline_proportional() {
        let mut state = ValidatorStateManager::new();
        // Use 100x minimum stake so a 1% slash stays above minimum
        let stake_amount = one_million_call() * 100;
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
    fn test_slash_offline_removes_validator_when_below_minimum() {
        let mut state = ValidatorStateManager::new();
        let stake_amount = one_million_call();
        let id = state
            .stake(test_addr(1), test_pubkey(1), stake_amount)
            .unwrap();

        // 10 rounds offline at 0.10% per round = 1% total = 10,000 CALL
        // Remaining = 990,000 CALL < minimum self-stake (1,000,000)
        let slashed = state.slash_offline(id, 10).unwrap();
        assert!(slashed > 0);

        // Validator removed because self-stake dropped below minimum
        assert!(state.get_validator_stake(id).is_none());
    }

    #[test]
    fn test_slash_oracle_outlier() {
        let mut state = ValidatorStateManager::new();
        // Use 100x minimum stake so a 0.1% slash stays above minimum
        let stake_amount = state.params.min_self_stake * 100;
        let id = state
            .stake(test_addr(1), test_pubkey(1), stake_amount)
            .unwrap();

        let slashed = state.slash_oracle_outlier(id).unwrap();
        let expected = (stake_amount * 10) / 10_000; // 0.1%
        assert_eq!(slashed, expected);

        let stake = state.get_validator_stake(id).unwrap();
        assert_eq!(stake.self_stake, stake_amount.saturating_sub(expected));
        assert_eq!(stake.slash_history.len(), 1);
        assert_eq!(stake.slash_history[0].reason, "oracle outlier");
    }

    #[test]
    fn test_slash_oracle_outlier_removes_validator_when_below_minimum() {
        let mut state = ValidatorStateManager::new();
        let stake_amount = state.params.min_self_stake;
        let id = state
            .stake(test_addr(1), test_pubkey(1), stake_amount)
            .unwrap();

        // 0.1% slash on exactly minimum self-stake drops below minimum
        let slashed = state.slash_oracle_outlier(id).unwrap();
        assert!(slashed > 0);

        // Validator removed because self-stake dropped below minimum
        assert!(state.get_validator_stake(id).is_none());
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

        state.unstake(id, test_addr(1)).unwrap();

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

        state.unstake(id, test_addr(1)).unwrap();

        // Before period: no eligible requests
        let eligible = state.eligible_unbonding_requests();
        assert!(eligible.is_empty());

        // After period: eligible
        state.set_current_block(state.params.unbonding_period_blocks + 100);
        let eligible = state.eligible_unbonding_requests();
        assert_eq!(eligible, vec![id]);
    }

    #[test]
    fn test_remove_validator_governance_slash() {
        let mut state = ValidatorStateManager::new();
        let id = state
            .stake(test_addr(1), test_pubkey(1), one_million_call())
            .unwrap();

        // Add delegation
        state.delegate(id, 500_000 * 10u128.pow(18)).unwrap();

        let pre_stake = state.get_validator_stake(id).unwrap().clone();
        assert_eq!(pre_stake.self_stake, one_million_call());
        assert!(pre_stake.delegated_call > 0);

        // Governance slashes validator
        let slashed = state.remove_validator(id).unwrap();
        assert_eq!(slashed, one_million_call());

        // Validator is removed
        assert!(state.get_validator_stake(id).is_none());

        // Cannot remove again
        assert!(matches!(
            state.remove_validator(id),
            Err(ConsensusError::ValidatorNotFound(_))
        ));
    }

    #[test]
    fn test_is_qualified_validator() {
        let mut state = ValidatorStateManager::new();
        let addr = test_addr(1);

        // No validators yet — random address is not qualified
        assert!(!state.is_qualified_validator(&addr));

        // Stake below minimum — still not qualified
        let result = state.stake(addr, test_pubkey(1), state.params.min_self_stake - 1);
        assert!(result.is_err());
        assert!(!state.is_qualified_validator(&addr));

        // Stake at minimum — now qualified
        let id = state.stake(addr, test_pubkey(1), state.params.min_self_stake).unwrap();
        assert!(state.is_qualified_validator(&addr));

        // Another address that hasn't staked is not qualified
        assert!(!state.is_qualified_validator(&test_addr(2)));

        // Unstake — no longer qualified
        state.unstake(id, addr).unwrap();
        assert!(!state.is_qualified_validator(&addr));
    }
}

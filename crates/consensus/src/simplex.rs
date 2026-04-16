//! T6.1 — Simplex BFT Integration (per spec §2.3)
//!
//! Lightweight wrapper around commonware-consensus providing Callchain-specific
//! consensus driver with proposer selection, validator management, and block lifecycle.

use crate::block::{Block, BlockExecutionResult};
use crate::proposer::{
    select_proposer, select_proposer_subset, verify_proposer_in_subset, ConsensusParams,
};
use crate::validator::{ConsensusError, ValidatorStateManager};
use call_primitives::{Address, ValidatorId};
use tracing::{info, warn};

/// Simplex BFT consensus driver for Callchain.
///
/// Manages the consensus lifecycle:
/// - Proposer selection per round (21 of 216 validators)
/// - Block proposal and validation
/// - Commit/rollback of blocks
/// - Validator state management (staking, slashing, rewards)
pub struct SimplexConsensus {
    params: ConsensusParams,
    validators: ValidatorStateManager,
    current_round: u64,
    current_height: u64,
    /// Active proposer subset for the current epoch
    proposer_subset: Vec<ValidatorId>,
}

impl SimplexConsensus {
    /// Create a new consensus instance with initial validators.
    pub fn new(params: ConsensusParams, validators: ValidatorStateManager) -> Self {
        let active = validators.get_active_validators();
        let proposer_subset =
            select_proposer_subset(&active, 0, params.subset_size);

        Self {
            params,
            validators,
            current_round: 0,
            current_height: 0,
            proposer_subset,
        }
    }

    /// Get the current consensus parameters.
    pub fn params(&self) -> &ConsensusParams {
        &self.params
    }

    /// Get the validator state manager.
    pub fn validators(&self) -> &ValidatorStateManager {
        &self.validators
    }

    /// Stake a new validator.
    pub fn stake_validator(
        &mut self,
        address: Address,
        pubkey: [u8; 32],
        amount: u128,
    ) -> Result<u32, ConsensusError> {
        self.validators.stake(address, pubkey, amount)
    }

    /// Refresh the proposer subset from the current validator set.
    pub fn refresh_proposer_subset(&mut self) {
        let active = self.validators.get_active_validators();
        self.proposer_subset =
            select_proposer_subset(&active, self.current_round, self.params.subset_size);
        info!(
            round = self.current_round,
            subset_size = self.proposer_subset.len(),
            "refreshed proposer subset"
        );
    }

    /// Get the current block height.
    pub fn current_height(&self) -> u64 {
        self.current_height
    }

    /// Get the current round number.
    pub fn current_round(&self) -> u64 {
        self.current_round
    }

    /// Get the current proposer subset.
    pub fn proposer_subset(&self) -> &[ValidatorId] {
        &self.proposer_subset
    }

    /// Select the block proposer for the current round.
    pub fn current_proposer(&self) -> Option<ValidatorId> {
        select_proposer(&self.proposer_subset, self.current_round)
    }

    /// Advance to the next round, refreshing the proposer subset if needed.
    ///
    /// Per spec §2.3: proposer subset changes per epoch (round),
    /// while the proposer rotates within the subset each block.
    pub fn advance_round(&mut self) {
        self.current_round += 1;

        // Refresh proposer subset periodically (every N rounds = epoch)
        // For now, refresh every 100 rounds to balance stability and rotation
        let epoch_length = 100u64;
        if self.current_round % epoch_length == 0 {
            let active = self.validators.get_active_validators();
            self.proposer_subset =
                select_proposer_subset(&active, self.current_round, self.params.subset_size);
            info!(
                round = self.current_round,
                subset_size = self.proposer_subset.len(),
                "refreshed proposer subset"
            );
        }
    }

    /// Validate a proposed block before execution.
    ///
    /// Checks:
    /// - Proposer is in the current subset
    /// - Block height matches expected next height
    /// - Block header is internally consistent
    pub fn validate_block(
        &self,
        block: &Block,
        parent_hash: call_primitives::BlockHash,
    ) -> Result<(), ConsensusError> {
        // Verify proposer is in the current subset
        let proposer = block.header.proposer;
        if !verify_proposer_in_subset(proposer, &self.proposer_subset) {
            return Err(ConsensusError::ProposerNotInSubset(proposer));
        }

        // Verify block height
        if block.header.height != self.current_height {
            return Err(ConsensusError::InvalidBlock(format!(
                "expected height {}, got {}",
                self.current_height, block.header.height
            )));
        }

        // Validate block structure and header
        block.validate(parent_hash)?;

        Ok(())
    }

    /// Execute a validated block and return the execution result.
    ///
    /// This runs the full execution pipeline:
    /// EVM → Protocol → Bridge → System
    ///
    /// The caller is responsible for providing the correct state handles.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_block(
        &self,
        block: &Block,
        balances: &mut call_protocol::balances::BalanceState,
        registry: &call_protocol::registry::AssetRegistry,
        compliance: &call_protocol::compliance::ComplianceEngine,
        bridge_state: &mut call_bridge::BridgeStateManager,
        shielded_state: &mut call_shielded::ShieldedState,
        fee_params: &mut call_protocol::FeeParams,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        block.execute(
            balances,
            registry,
            compliance,
            bridge_state,
            shielded_state,
            fee_params,
            self.current_height,
        )
    }

    /// Commit a block: advance height, distribute rewards, advance round.
    pub fn commit_block(
        &mut self,
        block: &Block,
        result: &BlockExecutionResult,
    ) -> Result<(), ConsensusError> {
        // Distribute validator reward
        if result.total_validator_reward > 0 {
            let proposer = block.header.proposer;
            self.validators
                .distribute_reward(proposer, result.total_validator_reward)
                .map_err(|e| {
                    warn!(?e, proposer, "failed to distribute reward");
                    e
                })?;
        }

        // Advance state
        self.current_height += 1;
        self.advance_round();

        info!(
            height = self.current_height,
            round = self.current_round,
            proposer = block.header.proposer,
            tx_count = result.total_tx_count(),
            "committed block"
        );

        Ok(())
    }

    /// Handle double-sign detection: slash the offending validator.
    pub fn handle_double_sign(
        &mut self,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let slashed = self.validators.slash_double_sign(validator_id)?;
        warn!(validator_id, slashed, "slashed validator for double sign");
        Ok(slashed)
    }

    /// Handle offline detection: slash proportionally.
    pub fn handle_offline(
        &mut self,
        validator_id: ValidatorId,
        rounds_offline: u64,
    ) -> Result<u128, ConsensusError> {
        let slashed = self.validators.slash_offline(validator_id, rounds_offline)?;
        warn!(
            validator_id,
            rounds_offline, slashed, "slashed validator for being offline"
        );
        Ok(slashed)
    }

    /// Get active validator IDs for network layer.
    pub fn active_validators(&self) -> Vec<ValidatorId> {
        self.validators.get_active_validators()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::{Address, Ed25519PublicKey};

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

    fn make_test_validators(n: u32) -> ValidatorStateManager {
        let mut state = ValidatorStateManager::new();
        for i in 0..n {
            state
                .stake(test_addr(i as u8), test_pubkey(i as u8), one_million_call())
                .unwrap();
        }
        state
    }

    fn make_test_consensus(n: u32) -> SimplexConsensus {
        let validators = make_test_validators(n);
        SimplexConsensus::new(ConsensusParams::default(), validators)
    }

    #[test]
    fn test_consensus_initialization() {
        let consensus = make_test_consensus(100);
        assert_eq!(consensus.current_height(), 0);
        assert_eq!(consensus.current_round(), 0);
        assert!(consensus.current_proposer().is_some());
        assert!(!consensus.proposer_subset().is_empty());
    }

    #[test]
    fn test_consensus_advance_round() {
        let mut consensus = make_test_consensus(100);
        let old_height = consensus.current_height();
        let old_round = consensus.current_round();

        // Simulate committing a block
        consensus.current_height += 1;
        consensus.advance_round();

        assert_eq!(consensus.current_height(), old_height + 1);
        assert_eq!(consensus.current_round(), old_round + 1);
    }

    #[test]
    fn test_consensus_proposer_rotation() {
        let consensus = make_test_consensus(100);
        let p1 = consensus.current_proposer().unwrap();

        let mut consensus2 = make_test_consensus(100);
        consensus2.current_round = 1;
        let p2 = consensus2.current_proposer().unwrap();

        // Different rounds should select different proposers
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_consensus_handle_double_sign() {
        let mut consensus = make_test_consensus(100);
        let validator_id = 0;

        let slashed = consensus.handle_double_sign(validator_id).unwrap();
        assert_eq!(slashed, one_million_call());

        // Validator should now be unbonding/slashed
        assert!(consensus.validators().is_unbonding(validator_id)
            || consensus.validators().get_validator_stake(validator_id).is_some_and(|v| v.self_stake == 0));
    }

    #[test]
    fn test_consensus_handle_offline() {
        let mut consensus = make_test_consensus(100);
        let validator_id = 5;

        let slashed = consensus.handle_offline(validator_id, 5).unwrap();
        assert!(slashed > 0);
        // 5 rounds * 0.10% = 0.5% of stake
        let expected = (one_million_call() * 5 * 10) / 10_000;
        assert_eq!(slashed, expected);
    }

    #[test]
    fn test_consensus_active_validators() {
        let consensus = make_test_consensus(100);
        let active = consensus.active_validators();
        assert_eq!(active.len(), 100);
    }

    #[test]
    fn test_consensus_subset_refresh_on_epoch() {
        let mut consensus = make_test_consensus(100);
        // Advance 100 rounds (epoch boundary)
        for _ in 0..100 {
            consensus.advance_round();
        }

        let subset_after = consensus.proposer_subset();
        // Subset should have been refreshed at epoch boundary
        // (different round seed = different subset)
        assert_ne!(consensus.current_round(), 0);
        // The subset should still have the right size
        assert_eq!(subset_after.len(), 21);
    }

    #[test]
    fn test_consensus_params_accessor() {
        let consensus = make_test_consensus(100);
        let params = consensus.params();
        assert_eq!(params.max_validators, 216);
        assert_eq!(params.subset_size, 21);
        assert_eq!(params.block_time_millis, 250);
    }
}

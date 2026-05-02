//! T6.1 — Simplex BFT Integration (per spec §2.3)
//!
//! Lightweight wrapper around commonware-consensus providing Callchain-specific
//! consensus driver with proposer selection, validator management, and block lifecycle.

use crate::block::{Block, BlockExecutionResult, ExecutionState, BlockContext, Subsystems};
use crate::fork::ForkManager;
use crate::proposer::{
    derive_vrf_seed, select_proposer, select_proposer_subset, verify_proposer_in_subset,
    ConsensusParams,
};
use crate::validator::ConsensusError;
use call_primitives::{Address, BlockHash, ValidatorId};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Simplex BFT consensus driver for Callchain.
///
/// Manages the consensus lifecycle:
/// - Proposer selection per epoch (21 of qualified validators) using VRF
/// - Block proposal and validation
/// - Commit/rollback of blocks
/// - Validator state management (staking, slashing, rewards)
pub struct SimplexConsensus {
    params: ConsensusParams,
    current_round: u64,
    current_height: u64,
    /// Active proposer subset for the current epoch
    proposer_subset: Vec<ValidatorId>,
    /// Hash of the last committed block, used as VRF seed input
    last_block_hash: BlockHash,
}

impl SimplexConsensus {
    /// Create a new consensus instance from EVM validator storage.
    pub fn new(params: ConsensusParams, evm_state: &call_evm::EvmState) -> Self {
        let active = Self::qualified_validators_internal(evm_state, &params);
        let pubkeys = Self::build_pubkey_map_internal(evm_state);
        let seed = derive_vrf_seed(&BlockHash::ZERO, 0);
        let proposer_subset =
            select_proposer_subset(&active, &pubkeys, &seed, params.subset_size);

        Self {
            params,
            current_round: 0,
            current_height: 0,
            proposer_subset,
            last_block_hash: BlockHash::ZERO,
        }
    }

    /// Build a map of validator ID → Ed25519 pubkey from EVM storage.
    fn build_pubkey_map_internal(
        evm_state: &call_evm::EvmState,
    ) -> std::collections::HashMap<ValidatorId, call_primitives::Ed25519PublicKey> {
        use crate::exec::evm_instructions::{
            read_validator_count, read_validator_addr, read_validator_pubkey,
        };
        let count = read_validator_count(evm_state);
        let mut map = std::collections::HashMap::new();
        for id in 1..=count {
            let addr = read_validator_addr(evm_state, id);
            if addr != Address::ZERO {
                let pk = read_validator_pubkey(evm_state, addr);
                map.insert(id as ValidatorId, pk);
            }
        }
        map
    }

    /// Recompute the proposer subset using the current VRF seed.
    fn recompute_proposer_subset(&mut self, evm_state: &call_evm::EvmState) {
        let active = Self::qualified_validators_internal(evm_state, &self.params);
        let pubkeys = Self::build_pubkey_map_internal(evm_state);
        let seed = derive_vrf_seed(&self.last_block_hash, self.current_round);
        self.proposer_subset =
            select_proposer_subset(&active, &pubkeys, &seed, self.params.subset_size);
    }

    /// Get the current consensus parameters.
    pub fn params(&self) -> &ConsensusParams {
        &self.params
    }

    /// Stake a new validator directly into EVM storage.
    pub fn stake_validator(
        &mut self,
        evm_state: &mut call_evm::EvmState,
        address: Address,
        pubkey: [u8; 32],
        amount: u128,
    ) -> Result<u64, ConsensusError> {
        if amount < self.params.min_self_stake {
            return Err(ConsensusError::InsufficientStake);
        }
        let id = crate::exec::evm_instructions::stake_validator_evm(
            evm_state, address, pubkey, amount,
        );
        Ok(id)
    }

    /// Refresh the proposer subset from EVM validator storage.
    pub fn refresh_proposer_subset(&mut self, evm_state: &call_evm::EvmState) {
        self.recompute_proposer_subset(evm_state);
        info!(
            round = self.current_round,
            subset_size = self.proposer_subset.len(),
            "refreshed proposer subset"
        );
    }

    /// Set the last committed block hash (used for VRF seed derivation).
    pub fn set_last_block_hash(&mut self, hash: BlockHash) {
        self.last_block_hash = hash;
    }

    /// Get the last committed block hash.
    pub fn last_block_hash(&self) -> BlockHash {
        self.last_block_hash
    }

    /// Get the current block height.
    pub fn current_height(&self) -> u64 {
        self.current_height
    }

    /// Set the current height (used during emergency rollback).
    pub fn set_current_height(&mut self, height: u64) {
        self.current_height = height;
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
    ///
    /// NOTE: Epoch churn (queued stake/exit processing) is currently a no-op.
    /// In the EVM-only architecture, churn should be handled by system
    /// transactions included at epoch boundaries.
    pub fn advance_round(&mut self, evm_state: &call_evm::EvmState) {
        self.current_round += 1;

        // Refresh proposer subset periodically (every N rounds = epoch)
        let epoch_length = self.params.epoch_length;
        if self.current_round.is_multiple_of(epoch_length) {
            self.recompute_proposer_subset(evm_state);
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
        fork_manager: &ForkManager,
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

        // Validate block structure and header (including protocol version)
        block.validate(parent_hash, fork_manager)?;

        Ok(())
    }

    /// Execute a validated block and return the execution result.
    ///
    /// This runs the full execution pipeline:
    /// EVM → Protocol → Bridge → System
    ///
    /// The caller is responsible for providing the correct state handles.
    pub fn execute_block(
        &mut self,
        block: &Block,
        fee_params: &mut call_protocol::FeeParams,
        evm_state: &mut call_evm::EvmState,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        block.execute(
            &mut ExecutionState::new(evm_state),
            &mut BlockContext::new(self.current_height, fee_params),
            &mut Subsystems::none(),
        )
    }

    /// Commit a block: advance height, distribute rewards, advance round.
    ///
    /// NOTE: Reward distribution writes directly to EVM storage. The caller
    /// must recompute the state root if this block's header has already been
    /// sealed. In the EVM-only architecture, rewards should eventually become
    /// system transactions included during block execution.
    pub fn commit_block(
        &mut self,
        block: &Block,
        result: &BlockExecutionResult,
        evm_state: &mut call_evm::EvmState,
    ) -> Result<(), ConsensusError> {
        // Replay protection: only commit blocks at the expected height
        if block.header.height != self.current_height {
            return Err(ConsensusError::InvalidBlock(format!(
                "height mismatch: expected {}, got {}",
                self.current_height, block.header.height
            )));
        }

        // Distribute validator reward by adding to the proposer's stake in EVM storage
        if result.total_validator_reward > 0 {
            let proposer_id = block.header.proposer;
            let proposer_addr = crate::exec::evm_instructions::read_validator_addr(evm_state, proposer_id as u64);
            if proposer_addr != Address::ZERO {
                crate::exec::evm_instructions::distribute_reward_evm(
                    evm_state, proposer_addr, result.total_validator_reward,
                );
            } else {
                warn!(proposer_id, "proposer not found in EVM storage, skipping reward");
            }
        }

        // Update last block hash for VRF seed derivation
        self.last_block_hash = block.header.hash();

        // Advance state
        self.current_height += 1;
        self.advance_round(evm_state);

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
        evm_state: &mut call_evm::EvmState,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let addr = crate::exec::evm_instructions::read_validator_addr(evm_state, validator_id as u64);
        if addr == Address::ZERO {
            return Err(ConsensusError::ValidatorNotFound(validator_id));
        }
        let slashed = crate::exec::evm_instructions::read_validator_stake(evm_state, addr);
        crate::exec::evm_instructions::slash_validator_evm(evm_state, addr, slashed);
        warn!(validator_id, slashed, "slashed validator for double sign");
        Ok(slashed)
    }

    /// Handle offline detection: slash proportionally.
    pub fn handle_offline(
        &mut self,
        evm_state: &mut call_evm::EvmState,
        validator_id: ValidatorId,
        rounds_offline: u64,
    ) -> Result<u128, ConsensusError> {
        let addr = crate::exec::evm_instructions::read_validator_addr(evm_state, validator_id as u64);
        if addr == Address::ZERO {
            return Err(ConsensusError::ValidatorNotFound(validator_id));
        }
        let self_stake = crate::exec::evm_instructions::read_validator_stake(evm_state, addr);
        let rate_total = self.params.offline_slash_rate_bps * rounds_offline as u128;
        let slashed = (self_stake * rate_total) / 10_000;
        crate::exec::evm_instructions::slash_validator_evm(evm_state, addr, slashed);
        warn!(
            validator_id,
            rounds_offline, slashed, "slashed validator for being offline"
        );
        Ok(slashed)
    }

    /// Handle oracle outlier detection: slash the offending validator.
    pub fn handle_oracle_outlier(
        &mut self,
        evm_state: &mut call_evm::EvmState,
        validator_id: ValidatorId,
    ) -> Result<u128, ConsensusError> {
        let addr = crate::exec::evm_instructions::read_validator_addr(evm_state, validator_id as u64);
        if addr == Address::ZERO {
            return Err(ConsensusError::ValidatorNotFound(validator_id));
        }
        let self_stake = crate::exec::evm_instructions::read_validator_stake(evm_state, addr);
        let slashed = (self_stake * 10) / 10_000; // 0.1%
        crate::exec::evm_instructions::slash_validator_evm(evm_state, addr, slashed);
        warn!(validator_id, slashed, "slashed validator for oracle outlier");
        Ok(slashed)
    }

    /// Distribute an oracle reward to a validator by adding to their EVM stake.
    pub fn distribute_oracle_reward(
        &mut self,
        evm_state: &mut call_evm::EvmState,
        validator_id: ValidatorId,
        amount: u128,
    ) -> Result<(), ConsensusError> {
        if amount > 0 {
            let addr = crate::exec::evm_instructions::read_validator_addr(evm_state, validator_id as u64);
            if addr == Address::ZERO {
                warn!(validator_id, "validator not found, skipping oracle reward");
                return Ok(());
            }
            crate::exec::evm_instructions::distribute_reward_evm(evm_state, addr, amount);
        }
        Ok(())
    }

    /// Get active validator IDs from EVM storage.
    pub fn active_validators(&self, evm_state: &call_evm::EvmState) -> Vec<ValidatorId> {
        use crate::exec::evm_instructions::{
            read_validator_count, read_validator_addr, read_validator_status,
        };
        let count = read_validator_count(evm_state);
        let mut active = Vec::new();
        for id in 1..=count {
            let addr = read_validator_addr(evm_state, id);
            if addr != Address::ZERO && read_validator_status(evm_state, addr) != 0 {
                active.push(id as ValidatorId);
            }
        }
        active
    }

    /// Get qualified validator IDs (stake ≥ MIN_SELF_STAKE, active) from EVM storage.
    fn qualified_validators_internal(
        evm_state: &call_evm::EvmState,
        params: &ConsensusParams,
    ) -> Vec<ValidatorId> {
        use crate::exec::evm_instructions::{
            read_validator_count, read_validator_addr, read_validator_stake, read_validator_status,
        };
        let count = read_validator_count(evm_state);
        let mut qualified = Vec::new();
        for id in 1..=count {
            let addr = read_validator_addr(evm_state, id);
            if addr != Address::ZERO && read_validator_status(evm_state, addr) != 0 {
                let stake = read_validator_stake(evm_state, addr);
                if stake >= params.min_self_stake {
                    qualified.push(id as ValidatorId);
                }
            }
        }
        qualified
    }

    /// Get qualified validator IDs (stake ≥ MIN_SELF_STAKE, active).
    pub fn qualified_validators(
        &self,
        evm_state: &call_evm::EvmState,
    ) -> Vec<ValidatorId> {
        Self::qualified_validators_internal(evm_state, &self.params)
    }
}

/// Serialized consensus state for database persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedConsensusState {
    pub current_height: u64,
    pub current_round: u64,
    pub proposer_subset: Vec<ValidatorId>,
    pub last_block_hash: BlockHash,
    pub params: ConsensusParams,
}

impl SimplexConsensus {
    /// Serialize the current consensus state for persistence.
    pub fn persist_state(&self) -> PersistedConsensusState {
        PersistedConsensusState {
            current_height: self.current_height,
            current_round: self.current_round,
            proposer_subset: self.proposer_subset.clone(),
            last_block_hash: self.last_block_hash,
            params: self.params,
        }
    }

    /// Restore consensus state from a persisted snapshot.
    ///
    /// Rebuilds the proposer subset from EVM validator storage.
    pub fn restore_from_persisted(
        state: PersistedConsensusState,
        evm_state: &call_evm::EvmState,
    ) -> Self {
        let active = Self::qualified_validators_internal(evm_state, &state.params);
        let pubkeys = Self::build_pubkey_map_internal(evm_state);
        let seed = derive_vrf_seed(&state.last_block_hash, state.current_round);
        let proposer_subset =
            select_proposer_subset(&active, &pubkeys, &seed, state.params.subset_size);

        Self {
            params: state.params,
            current_round: state.current_round,
            current_height: state.current_height,
            proposer_subset,
            last_block_hash: state.last_block_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::{Address, Ed25519PublicKey};
    use call_evm::EvmState;
    use crate::exec::evm_instructions::seed_validator;

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

    fn make_test_evm_state(n: u32) -> EvmState {
        let mut evm = EvmState::new();
        for i in 0..n {
            seed_validator(
                &mut evm,
                (i + 1) as u64,
                test_addr((i + 1) as u8),
                test_pubkey(i as u8),
                one_million_call(),
                1,
            );
        }
        evm
    }

    fn make_test_consensus(n: u32) -> (SimplexConsensus, EvmState) {
        let evm = make_test_evm_state(n);
        let consensus = SimplexConsensus::new(ConsensusParams::default(), &evm);
        (consensus, evm)
    }

    #[test]
    fn test_consensus_initialization() {
        let (consensus, _evm) = make_test_consensus(100);
        assert_eq!(consensus.current_height(), 0);
        assert_eq!(consensus.current_round(), 0);
        assert!(consensus.current_proposer().is_some());
        assert!(!consensus.proposer_subset().is_empty());
    }

    #[test]
    fn test_consensus_advance_round() {
        let (mut consensus, evm) = make_test_consensus(100);
        let old_height = consensus.current_height();
        let old_round = consensus.current_round();

        // Simulate committing a block
        consensus.current_height += 1;
        consensus.advance_round(&evm);

        assert_eq!(consensus.current_height(), old_height + 1);
        assert_eq!(consensus.current_round(), old_round + 1);
    }

    #[test]
    fn test_consensus_proposer_rotation() {
        let (consensus, _evm) = make_test_consensus(100);
        let p1 = consensus.current_proposer().unwrap();

        let (mut consensus2, _evm2) = make_test_consensus(100);
        consensus2.current_round = 1;
        let p2 = consensus2.current_proposer().unwrap();

        // Different rounds should select different proposers
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_consensus_handle_double_sign() {
        let (mut consensus, mut evm) = make_test_consensus(100);
        let validator_id = 1; // IDs are 1-based in EVM storage

        let slashed = consensus.handle_double_sign(&mut evm, validator_id).unwrap();
        assert_eq!(slashed, one_million_call());

        // Validator should be removed from the active set after double-sign slash
        let active = consensus.active_validators(&evm);
        assert!(!active.contains(&validator_id));
    }

    #[test]
    fn test_consensus_handle_offline() {
        let (mut consensus, mut evm) = make_test_consensus(100);
        let validator_id = 6; // IDs are 1-based

        let slashed = consensus.handle_offline(&mut evm, validator_id, 5).unwrap();
        assert!(slashed > 0);
        // 5 rounds * 0.10% = 0.5% of stake
        let expected = (one_million_call() * 5 * 10) / 10_000;
        assert_eq!(slashed, expected);
    }

    #[test]
    fn test_consensus_active_validators() {
        let (consensus, evm) = make_test_consensus(100);
        let active = consensus.active_validators(&evm);
        assert_eq!(active.len(), 100);
    }

    #[test]
    fn test_consensus_subset_refresh_on_epoch() {
        let (mut consensus, evm) = make_test_consensus(100);
        // Advance 100 rounds (epoch boundary)
        for _ in 0..100 {
            consensus.advance_round(&evm);
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
        let (consensus, _evm) = make_test_consensus(100);
        let params = consensus.params();
        assert_eq!(params.max_validators, 216);
        assert_eq!(params.subset_size, 21);
        assert_eq!(params.block_time_millis, 250);
    }

    #[test]
    fn test_consensus_persist_and_restore() {
        let (mut consensus, evm) = make_test_consensus(100);
        consensus.current_height = 42;
        consensus.current_round = 7;
        consensus.last_block_hash = BlockHash::repeat_byte(0xAB);

        let persisted = consensus.persist_state();
        let data = serde_json::to_vec(&persisted).unwrap();
        let loaded: PersistedConsensusState = serde_json::from_slice(&data).unwrap();

        let restored = SimplexConsensus::restore_from_persisted(loaded, &evm);

        assert_eq!(restored.current_height(), 42);
        assert_eq!(restored.current_round(), 7);
        assert_eq!(restored.last_block_hash(), BlockHash::repeat_byte(0xAB));
    }

}

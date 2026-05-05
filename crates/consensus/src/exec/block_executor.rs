//! EVM-backed implementation of [`CallchainBlockExecutor`].
//!
//! Delegates all operations to `state_accessors` — the same helpers
//! previously called directly from `SimplexConsensus`.

use call_evm::CallchainBlockExecutor;
use call_primitives::Address;

/// Default executor: applies consensus mutations via EVM state accessors.
#[derive(Debug, Default, Clone)]
pub struct EvmBlockExecutor;

impl EvmBlockExecutor {
    /// Create a new EVM block executor.
    pub fn new() -> Self {
        Self
    }
}

impl CallchainBlockExecutor for EvmBlockExecutor {
    fn stake_validator(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        address: Address,
        pubkey: [u8; 32],
        amount: u128,
    ) -> u64 {
        crate::exec::state_accessors::stake_validator_evm(state, address, pubkey, amount)
    }

    fn distribute_block_reward(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        proposer_addr: Address,
        amount: u128,
    ) {
        crate::exec::state_accessors::distribute_reward_evm(state, proposer_addr, amount);
    }

    fn slash_double_sign(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        validator_addr: Address,
        amount: u128,
    ) {
        crate::exec::state_accessors::slash_validator_evm(state, validator_addr, amount);
    }

    fn slash_offline(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        validator_addr: Address,
        amount: u128,
    ) {
        crate::exec::state_accessors::slash_validator_evm(state, validator_addr, amount);
    }

    fn slash_oracle_outlier(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        validator_addr: Address,
        amount: u128,
    ) {
        crate::exec::state_accessors::slash_validator_evm(state, validator_addr, amount);
    }

    fn distribute_oracle_reward(
        &mut self,
        state: &mut dyn call_evm::ProtocolStorage,
        validator_addr: Address,
        amount: u128,
    ) {
        crate::exec::state_accessors::distribute_reward_evm(state, validator_addr, amount);
    }
}

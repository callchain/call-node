//! Unified state access bundles for `RpcState`.
//!
//! `StateWriteBundle` and `StateReadBundle` acquire all state locks in a
//! deterministic order (matching the field declaration order in `RpcState`)
//! to eliminate the repetitive ~12-line lock-acquisition pattern that was
//! repeated 14+ times across the node crate, and to make it impossible to
//! forget a subsystem when calling `block.execute()`.

use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use call_bridge::BridgeConfig;
use call_consensus::{
    block::{BlockContext, ExecutionState, Subsystems},
    Block, BlockExecutionResult, ConsensusError, ForkManager,
};
use call_evm::EvmState;
use call_protocol::gas::FeeParams;

use crate::handlers::RpcState;

/// Holds write guards for every state component in `RpcState`.
///
/// Locks are acquired in the same deterministic order every time to avoid
/// deadlocks.  Always acquire through [`RpcState::write_all`].
pub struct StateWriteBundle<'a> {
    pub evm: RwLockWriteGuard<'a, EvmState>,
    pub fee_params: RwLockWriteGuard<'a, FeeParams>,
    pub fork_manager: RwLockWriteGuard<'a, ForkManager>,
}

/// Holds read guards for every state component in `RpcState`.
///
/// Useful for lightweight read-only operations or for cloning state before
/// the `propose` / `verify` phases of BFT consensus.
pub struct StateReadBundle<'a> {
    pub evm: RwLockReadGuard<'a, EvmState>,
    pub fee_params: RwLockReadGuard<'a, FeeParams>,
    pub fork_manager: RwLockReadGuard<'a, ForkManager>,
}

impl RpcState {
    /// Acquire write locks on **all** state components in deterministic order.
    ///
    /// # Panics
    /// Panics if any lock is poisoned (a previous holder panicked while holding
    /// the lock).  In practice this should never happen in normal node operation.
    pub fn write_all(&self) -> StateWriteBundle<'_> {
        StateWriteBundle {
            evm: self.evm_state.write().unwrap(),
            fee_params: self.fee_params.write().unwrap(),
            fork_manager: self.fork_manager.write().unwrap(),
        }
    }

    /// Acquire read locks on **all** state components in deterministic order.
    pub fn read_all(&self) -> StateReadBundle<'_> {
        StateReadBundle {
            evm: self.evm_state.read().unwrap(),
            fee_params: self.fee_params.read().unwrap(),
            fork_manager: self.fork_manager.read().unwrap(),
        }
    }
}

impl<'a> StateWriteBundle<'a> {
    /// Execute a block against the state held in this bundle, passing **all**
    /// available subsystems (oracle, governance, validator, agents, forks).
    ///
    /// This is the canonical execution path for production code paths such as
    /// `apply_synced_blocks`, `bft_event_loop` finalize, and
    /// `block_production_loop`.
    pub fn execute_block(
        &mut self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let bridge_config = BridgeConfig::default();
        let validators = call_consensus::exec::state_accessors::read_validator_addresses(&self.evm);
        block.execute(
            &mut ExecutionState::new(
                &mut self.evm,
            ),
            &mut BlockContext {
                current_block_height: height,
                fee_params: &mut self.fee_params,
                bridge_config: Some(&bridge_config),
                validators: if validators.is_empty() { None } else { Some(&validators) },
            },
            &mut Subsystems {
                fork_manager: Some(&mut self.fork_manager),
            },
        )
    }

    /// Execute a block with **no** subsystems enabled.
    ///
    /// Useful for tests that only exercise basic transfers or empty blocks.
    pub fn execute_block_no_subsystems(
        &mut self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        block.execute(
            &mut ExecutionState::new(
                &mut self.evm,
            ),
            &mut BlockContext::new(height, &mut self.fee_params),
            &mut Subsystems::none(),
        )
    }
}

impl<'a> StateReadBundle<'a> {
    /// Execute a block against **cloned** copies of the read state.
    ///
    /// This is used in the BFT `propose` and `verify` phases where state must
    /// not be mutated.  Each component is `.clone()`'d before execution.
    pub fn execute_block_cloned(
        &self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut fee_params = self.fee_params.clone();
        let mut evm = self.evm.clone();
        let mut fork_manager = self.fork_manager.clone();
        let bridge_config = BridgeConfig::default();
        let validators = call_consensus::exec::state_accessors::read_validator_addresses(&self.evm);

        block.execute(
            &mut ExecutionState::new(
                &mut evm,
            ),
            &mut BlockContext {
                current_block_height: height,
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: if validators.is_empty() { None } else { Some(&validators) },
            },
            &mut Subsystems {
                fork_manager: Some(&mut fork_manager),
            },
        )
    }

    /// Execute a block against cloned state with **no** subsystems.
    pub fn execute_block_cloned_no_subsystems(
        &self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut fee_params = self.fee_params.clone();
        let mut evm = self.evm.clone();

        block.execute(
            &mut ExecutionState::new(
                &mut evm,
            ),
            &mut BlockContext::new(height, &mut fee_params),
            &mut Subsystems::none(),
        )
    }
}

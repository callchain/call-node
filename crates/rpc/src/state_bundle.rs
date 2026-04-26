//! Unified state access bundles for `RpcState`.
//!
//! `StateWriteBundle` and `StateReadBundle` acquire all state locks in a
//! deterministic order (matching the field declaration order in `RpcState`)
//! to eliminate the repetitive ~12-line lock-acquisition pattern that was
//! repeated 14+ times across the node crate, and to make it impossible to
//! forget a subsystem when calling `block.execute()`.

use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use call_agent::{AgentBalances, AgentRegistry};
use call_bridge::{BridgeConfig, BridgeStateManager};
use call_consensus::{
    block::{BlockContext, ExecutionState, Subsystems},
    Block, BlockExecutionResult, ConsensusError, ForkManager, ValidatorStateManager,
};
use call_evm::EvmState;
use call_governance::GovernanceManager;
use call_oracle::OracleManager;
use call_primitives::Address;
use call_protocol::{AccountState, AssetRegistry, ComplianceEngine, FeeParams};
use call_shielded::ShieldedState;

use crate::handlers::RpcState;

/// Holds write guards for every state component in `RpcState`.
///
/// Locks are acquired in the same deterministic order every time to avoid
/// deadlocks.  Always acquire through [`RpcState::write_all`].
pub struct StateWriteBundle<'a> {
    pub balances: RwLockWriteGuard<'a, AccountState>,
    pub registry: RwLockWriteGuard<'a, AssetRegistry>,
    pub compliance: RwLockWriteGuard<'a, ComplianceEngine>,
    pub evm: RwLockWriteGuard<'a, EvmState>,
    pub bridge: RwLockWriteGuard<'a, BridgeStateManager>,
    pub validator_state: RwLockWriteGuard<'a, ValidatorStateManager>,
    pub agent_registry: RwLockWriteGuard<'a, AgentRegistry>,
    pub agent_balances: RwLockWriteGuard<'a, AgentBalances>,
    pub shielded: RwLockWriteGuard<'a, ShieldedState>,
    pub fee_params: RwLockWriteGuard<'a, FeeParams>,
    pub governance: RwLockWriteGuard<'a, GovernanceManager>,
    pub oracle: RwLockWriteGuard<'a, OracleManager>,
    pub fork_manager: RwLockWriteGuard<'a, ForkManager>,
}

/// Holds read guards for every state component in `RpcState`.
///
/// Useful for lightweight read-only operations or for cloning state before
/// the `propose` / `verify` phases of BFT consensus.
pub struct StateReadBundle<'a> {
    pub balances: RwLockReadGuard<'a, AccountState>,
    pub registry: RwLockReadGuard<'a, AssetRegistry>,
    pub compliance: RwLockReadGuard<'a, ComplianceEngine>,
    pub evm: RwLockReadGuard<'a, EvmState>,
    pub bridge: RwLockReadGuard<'a, BridgeStateManager>,
    pub validator_state: RwLockReadGuard<'a, ValidatorStateManager>,
    pub agent_registry: RwLockReadGuard<'a, AgentRegistry>,
    pub agent_balances: RwLockReadGuard<'a, AgentBalances>,
    pub shielded: RwLockReadGuard<'a, ShieldedState>,
    pub fee_params: RwLockReadGuard<'a, FeeParams>,
    pub governance: RwLockReadGuard<'a, GovernanceManager>,
    pub oracle: RwLockReadGuard<'a, OracleManager>,
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
            balances: self.balance_state.write().unwrap(),
            registry: self.asset_registry.write().unwrap(),
            compliance: self.compliance_engine.write().unwrap(),
            evm: self.evm_state.write().unwrap(),
            bridge: self.bridge_state.write().unwrap(),
            validator_state: self.validator_state.write().unwrap(),
            agent_registry: self.agent_registry.write().unwrap(),
            agent_balances: self.agent_balances.write().unwrap(),
            shielded: self.shielded_state.write().unwrap(),
            fee_params: self.fee_params.write().unwrap(),
            governance: self.governance.write().unwrap(),
            oracle: self.oracle.write().unwrap(),
            fork_manager: self.fork_manager.write().unwrap(),
        }
    }

    /// Acquire read locks on **all** state components in deterministic order.
    pub fn read_all(&self) -> StateReadBundle<'_> {
        StateReadBundle {
            balances: self.balance_state.read().unwrap(),
            registry: self.asset_registry.read().unwrap(),
            compliance: self.compliance_engine.read().unwrap(),
            evm: self.evm_state.read().unwrap(),
            bridge: self.bridge_state.read().unwrap(),
            validator_state: self.validator_state.read().unwrap(),
            agent_registry: self.agent_registry.read().unwrap(),
            agent_balances: self.agent_balances.read().unwrap(),
            shielded: self.shielded_state.read().unwrap(),
            fee_params: self.fee_params.read().unwrap(),
            governance: self.governance.read().unwrap(),
            oracle: self.oracle.read().unwrap(),
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
        let validators: Vec<Address> = self.validator_state
            .get_all_validators()
            .values()
            .map(|v| v.address)
            .collect();
        block.execute(
            &mut ExecutionState::new(
                &mut self.balances,
                &mut self.registry,
                &mut self.compliance,
                &mut self.bridge,
                &mut self.shielded,
                &mut self.evm,
            ),
            &mut BlockContext {
                current_block_height: height,
                fee_params: &mut self.fee_params,
                bridge_config: Some(&bridge_config),
                validators: if validators.is_empty() { None } else { Some(&validators) },
            },
            &mut Subsystems {
                oracle: Some(&mut self.oracle),
                agent_balances: Some(&mut self.agent_balances),
                agent_registry: Some(&mut self.agent_registry),
                validator_state: Some(&mut self.validator_state),
                governance: Some(&mut self.governance),
                fork_manager: Some(&mut self.fork_manager),
                ..Subsystems::none()
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
                &mut self.balances,
                &mut self.registry,
                &mut self.compliance,
                &mut self.bridge,
                &mut self.shielded,
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
        let mut balances = self.balances.clone();
        let mut registry = self.registry.clone();
        let mut compliance = self.compliance.clone();
        let mut bridge = self.bridge.clone();
        let mut shielded = self.shielded.clone();
        let mut fee_params = self.fee_params.clone();
        let mut evm = self.evm.clone();
        let mut oracle = self.oracle.clone();
        let mut agent_balances = self.agent_balances.clone();
        let mut agent_registry = self.agent_registry.clone();
        let mut validator_state = self.validator_state.clone();
        let mut governance = self.governance.clone();
        let mut fork_manager = self.fork_manager.clone();
        let bridge_config = BridgeConfig::default();
        let validators: Vec<Address> = self.validator_state
            .get_all_validators()
            .values()
            .map(|v| v.address)
            .collect();

        block.execute(
            &mut ExecutionState::new(
                &mut balances,
                &mut registry,
                &mut compliance,
                &mut bridge,
                &mut shielded,
                &mut evm,
            ),
            &mut BlockContext {
                current_block_height: height,
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: if validators.is_empty() { None } else { Some(&validators) },
            },
            &mut Subsystems {
                oracle: Some(&mut oracle),
                agent_balances: Some(&mut agent_balances),
                agent_registry: Some(&mut agent_registry),
                validator_state: Some(&mut validator_state),
                governance: Some(&mut governance),
                fork_manager: Some(&mut fork_manager),
                ..Subsystems::none()
            },
        )
    }

    /// Execute a block against cloned state with **no** subsystems.
    pub fn execute_block_cloned_no_subsystems(
        &self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut balances = self.balances.clone();
        let mut registry = self.registry.clone();
        let mut compliance = self.compliance.clone();
        let mut bridge = self.bridge.clone();
        let mut shielded = self.shielded.clone();
        let mut fee_params = self.fee_params.clone();
        let mut evm = self.evm.clone();

        block.execute(
            &mut ExecutionState::new(
                &mut balances,
                &mut registry,
                &mut compliance,
                &mut bridge,
                &mut shielded,
                &mut evm,
            ),
            &mut BlockContext::new(height, &mut fee_params),
            &mut Subsystems::none(),
        )
    }
}

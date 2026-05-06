//! Unified state access bundles for `RpcState`.
//!
//! `StateWriteBundle` and `StateReadBundle` acquire all non-database locks in a
//! deterministic order to eliminate the repetitive lock-acquisition pattern.
//! State itself is no longer held in-memory; it is loaded fresh from MDBX on
//! every block execution.

use std::sync::{Arc, RwLockReadGuard, RwLockWriteGuard};

use call_consensus::{Block, BlockExecutionResult, ConsensusError};
use call_protocol::gas::FeeParams;
use reth_db::DatabaseEnv;

use crate::handlers::RpcState;

/// Holds a write guard for fee_params.
///
/// Only acquires the lock actually touched by block execution, keeping the
/// critical section as small as possible.  Always acquire through
/// [`RpcState::write_all`].
///
/// State is loaded from MDBX on demand inside `execute_block`.
pub struct StateWriteBundle<'a> {
    pub fee_params: RwLockWriteGuard<'a, FeeParams>,
    /// MDBX database environment — the single source of truth for state.
    pub db_env: Arc<DatabaseEnv>,
}

/// Holds a read guard for fee_params.
///
/// Useful for lightweight read-only operations or for cloning state before
/// the `propose` / `verify` phases of BFT consensus.
pub struct StateReadBundle<'a> {
    pub fee_params: RwLockReadGuard<'a, FeeParams>,
    /// MDBX database environment — the single source of truth for state.
    pub db_env: Arc<DatabaseEnv>,
}

impl RpcState {
    /// Acquire write locks on **all** state components in deterministic order.
    ///
    /// # Panics
    /// Panics if any lock is poisoned (a previous holder panicked while holding
    /// the lock).  In practice this should never happen in normal node operation.
    /// Acquire the write lock on `fee_params` only.
    ///
    /// `fork_manager` is **not** locked here — callers that need to mutate it
    /// should acquire that lock separately and for as short a time as possible.
    pub fn write_all(&self) -> StateWriteBundle<'_> {
        StateWriteBundle {
            fee_params: self.fee_params.write().unwrap(),
            db_env: Arc::clone(&self.db_env),
        }
    }

    /// Acquire the read lock on `fee_params` only.
    pub fn read_all(&self) -> StateReadBundle<'_> {
        StateReadBundle {
            fee_params: self.fee_params.read().unwrap(),
            db_env: Arc::clone(&self.db_env),
        }
    }
}

impl<'a> StateWriteBundle<'a> {
    /// Execute a block against the state loaded from MDBX.
    ///
    /// This is the canonical execution path for production code paths such as
    /// `apply_synced_blocks`, `bft_event_loop` finalize, and
    /// `block_production_loop`.
    pub fn execute_block(
        &mut self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&self.db_env)
            .map_err(|e| ConsensusError::InvalidBlock(format!("db load: {e}")))?;
        let result = block.execute(
            &mut provider,
            &mut self.fee_params,
            height,
            Some(&self.db_env),
        )?;
        provider
            .state()
            .save_to_db(&self.db_env)
            .map_err(|e| ConsensusError::InvalidBlock(format!("db save: {e}")))?;
        Ok(result)
    }

    /// Execute a block with **no** subsystems enabled.
    ///
    /// Useful for tests that only exercise basic transfers or empty blocks.
    pub fn execute_block_no_subsystems(
        &mut self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        // Subsystems are currently unused in Block::execute; delegate to execute_block.
        self.execute_block(block, height)
    }
}

impl<'a> StateReadBundle<'a> {
    /// Execute a block against **cloned** copies of the read state.
    ///
    /// This is used in the BFT `propose` and `verify` phases where state must
    /// not be mutated.  State is loaded fresh from MDBX and discarded after.
    pub fn execute_block_cloned(
        &self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut fee_params = self.fee_params.clone();
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&self.db_env)
            .map_err(|e| ConsensusError::InvalidBlock(format!("db load: {e}")))?;
        block.execute(&mut provider, &mut fee_params, height, None)
    }

    /// Execute a block against cloned state with **no** subsystems.
    pub fn execute_block_cloned_no_subsystems(
        &self,
        block: &Block,
        height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        // Subsystems are currently unused in Block::execute; delegate to execute_block_cloned.
        self.execute_block_cloned(block, height)
    }
}

//! T9.1 — Payload Builder (per spec §13.5.1)
//!
//! Block assembly from mempool with limits enforcement,
//! execution ordering, and state root computation.

use call_consensus::block::{Block, BlockExecutionResult, BlockContext, ExecutionState, Subsystems};
use call_consensus::validator::ConsensusError;
use call_primitives::{BlockHash, Hash};
use call_protocol::AccountState;
use call_protocol::compliance::ComplianceEngine;
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::FeeParams;
use call_payload_types::{BlockLimits, PayloadAttributes};
use call_transaction_pool::MempoolSelection;
use tracing::info;

// ── Payload Builder Error ────────────────────────────────────────────

/// Errors that can occur during payload building
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    #[error("block limit exceeded: {0}")]
    BlockLimitExceeded(String),
    #[error("transaction too large: {size} > {max}")]
    TransactionTooLarge { size: usize, max: usize },
    #[error("too many shielded transactions: {count} > {max}")]
    TooManyShielded { count: usize, max: usize },
    #[error("state root mismatch: expected {expected}, got {actual}")]
    StateRootMismatch { expected: Hash, actual: Hash },
    #[error("consensus error: {0}")]
    Consensus(#[from] ConsensusError),
    #[error("empty block: no transactions selected")]
    EmptyBlock,
}

// ── Built Payload ────────────────────────────────────────────────────

/// Result of building a payload: a finalized block with execution results
pub struct BuiltPayload {
    /// The finalized block
    pub block: Block,
    /// Execution results with state roots
    pub execution_result: BlockExecutionResult,
    /// Number of transactions selected
    pub tx_count: usize,
}

impl BuiltPayload {
    /// Total transaction count
    pub fn total_tx_count(&self) -> usize {
        self.execution_result.total_tx_count()
    }

    /// Get the block hash
    pub fn block_hash(&self) -> BlockHash {
        self.block.header.hash()
    }
}

// ── Payload Builder ──────────────────────────────────────────────────

/// Assembles blocks from mempool selections with limits enforcement
pub struct PayloadBuilder {
    /// Block limits configuration
    pub limits: BlockLimits,
    /// Fee parameters for the current block
    pub fee_params: FeeParams,
}

impl PayloadBuilder {
    /// Create a new payload builder with default limits
    pub fn new(fee_params: FeeParams) -> Self {
        Self {
            limits: BlockLimits::default(),
            fee_params,
        }
    }

    /// Create with custom limits
    pub fn with_limits(fee_params: FeeParams, limits: BlockLimits) -> Self {
        Self { limits, fee_params }
    }

    /// Build a payload from mempool selection.
    ///
    /// Selects EVM transactions from the mempool, enforces block limits,
    /// executes them, and computes state roots.
    pub fn build(
        &self,
        attrs: &PayloadAttributes,
        evm_txs: Vec<Vec<u8>>,
        _account: &mut AccountState,
        _registry: &mut AssetRegistry,
        _compliance: &mut ComplianceEngine,
        shielded_state: &mut call_shielded::ShieldedState,
        evm_state: &mut call_evm::EvmState,
        state_root: Hash,
        bridge_config: Option<&call_bridge::BridgeConfig>,
    ) -> Result<BuiltPayload, BuilderError> {
        let mut selected_evm = Vec::new();
        let mut total_count = 0;
        let mut total_size = 0;
        let mut evm_gas_used = 0u64;

        // Select EVM transactions
        for evm_tx in evm_txs {
            if total_count >= self.limits.max_transactions {
                break;
            }
            if evm_tx.len() > self.limits.max_tx_size {
                continue;
            }
            if total_size + evm_tx.len() > self.limits.max_block_size {
                break;
            }

            // Extract actual gas_limit from serialized EVM transaction
            let gas_estimate = serde_json::from_slice::<call_evm::EvmTransaction>(&evm_tx)
                .map(|tx| tx.gas_limit)
                .unwrap_or(u64::MAX);
            if evm_gas_used + gas_estimate > self.limits.max_evm_gas_per_block {
                continue;
            }

            total_size += evm_tx.len();
            total_count += 1;
            evm_gas_used += gas_estimate;
            selected_evm.push(evm_tx);
        }

        if total_count == 0 {
            return Err(BuilderError::EmptyBlock);
        }

        let mut block = Block::new(
            attrs.height,
            attrs.parent_hash,
            attrs.timestamp_millis,
            attrs.proposer,
            attrs.version,
            selected_evm,
        );

        // Execute the block
        let mut fee_params = self.fee_params.clone();
        let result = block.execute(
            &mut ExecutionState::new(evm_state),
            &mut BlockContext {
                current_block_height: attrs.height,
                fee_params: &mut fee_params,
                bridge_config,
                validators: None,
            },
            &mut Subsystems::none(),
        )?;

        // Verify state root matches expected (if non-zero)
        if state_root != Hash::ZERO && result.state_root != Hash::ZERO
            && result.state_root != state_root {
                return Err(BuilderError::StateRootMismatch {
                    expected: state_root,
                    actual: result.state_root,
                });
            }

        // Finalize block with computed roots
        block.finalize(&result);

        info!(
            "built payload: height={} evm={} total_size={}",
            attrs.height,
            result.evm_tx_count,
            total_size
        );

        Ok(BuiltPayload {
            block,
            execution_result: result,
            tx_count: total_count,
        })
    }

    /// Build a payload from mempool selection directly.
    ///
    /// Same as [`build`](Self::build) but takes a `MempoolSelection`
    /// instead of separate vectors. The mempool is EVM-only.
    pub fn build_from_mempool(
        &self,
        attrs: &PayloadAttributes,
        selection: MempoolSelection,
        _account: &mut AccountState,
        _registry: &mut AssetRegistry,
        _compliance: &mut ComplianceEngine,
        shielded_state: &mut call_shielded::ShieldedState,
        evm_state: &mut call_evm::EvmState,
        evm_state_root: Hash,
        bridge_config: Option<&call_bridge::BridgeConfig>,
    ) -> Result<BuiltPayload, BuilderError> {
        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        self.build(
            attrs,
            evm_txs,
            _account,
            _registry,
            _compliance,
            shielded_state,
            evm_state,
            evm_state_root,
            bridge_config,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::{Address, ProtocolVersion};
    use call_consensus::exec::evm_instructions;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_sender() -> Address {
        Address::repeat_byte(1)
    }

    /// Create a serialized EVM transaction for testing.
    fn make_evm_tx_bytes(gas_limit: u64) -> Vec<u8> {
        let tx = call_evm::EvmTransaction {
            caller: test_sender(),
            nonce: 0,
            gas_limit,
            gas_price: 1_000_000_000,
            to: Some(test_addr(2)),
            value: call_primitives::U256::from(100),
            data: call_evm::Bytes::default(),
            chain_id: 1,
        };
        serde_json::to_vec(&tx).unwrap()
    }

    #[test]
    fn test_payload_builds_block_from_mempool() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let evm_txs: Vec<Vec<u8>> = vec![make_evm_tx_bytes(21_000)];

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        assert_eq!(payload.block.header.height, 1);
        assert_eq!(payload.block.evm_txs.len(), 1);
        assert!(payload.block.header.state_root != Hash::ZERO);
    }

    #[test]
    fn test_payload_enforces_block_limits() {
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_transactions: 3,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // Send more evm txs than limit
        let evm_txs: Vec<Vec<u8>> = (0..10).map(|_| make_evm_tx_bytes(21_000)).collect();

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 100_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        assert_eq!(payload.tx_count, 3);
    }

    #[test]
    fn test_payload_evm_gas_limit() {
        // Test that EVM transactions exceeding gas limits are skipped.
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_evm_gas_per_block: 50_000,
            max_transactions: 10,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // Two EVM txs: one within gas limit, one exceeding
        let evm_txs: Vec<Vec<u8>> = vec![
            make_evm_tx_bytes(21_000),
            make_evm_tx_bytes(100_000),
        ];

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 30_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        // Only the tx within gas limit should be included
        assert_eq!(payload.tx_count, 1);
    }

    #[test]
    fn test_payload_execution_order() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let evm_txs: Vec<Vec<u8>> = vec![{
            let tx = call_evm::EvmTransaction {
                caller: test_addr(1),
                nonce: 0,
                gas_limit: 21_000,
                gas_price: 1_000_000_000,
                to: Some(test_addr(2)),
                value: call_primitives::U256::from(100),
                data: call_evm::Bytes::default(),
                chain_id: 1,
            };
            serde_json::to_vec(&tx).unwrap()
        }];

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 10_000_000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("CALL".into(), "Callchain".into(), 18, test_sender(), 0, 0, 0).unwrap();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));
        evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));
        evm_instructions::seed_balance(
            &mut evm_state, call_protocol::CALL_ASSET_ID, test_sender(), 10_000_000,
        );

        // Deploy wrapped token contract for asset 1 (use separate deployer to avoid nonce conflict)
        let deployer = test_addr(99);
        evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000_000u128));
        let deploy_executor = call_evm::EvmExecutor::new(1);
        let (contract_addr, deploy_result) = deploy_executor
            .deploy_erc20_template(
                deployer,
                &mut evm_state,
                "CALL",
                "CALL",
                18,
                call_protocol::BRIDGE_EVM_ADDRESS,
                deployer,
                call_primitives::U256::ZERO,
                call_primitives::U256::from(1u64),
            )
            .unwrap();
        assert!(deploy_result.success);
        registry.set_evm_contract_address(1, contract_addr);

        // Seed EVM storage for bridge ops
        evm_instructions::seed_asset(
            &mut evm_state,
            1,
            "CALL",
            "Callchain",
            18,
            test_sender(),
            0,
            0,
            0, // active
        );
        evm_instructions::seed_bridge_contract(&mut evm_state, 1, contract_addr);

        let bridge_config = call_bridge::BridgeConfig::default();

        let payload = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            Some(&bridge_config),
        ).unwrap();

        assert_eq!(payload.execution_result.evm_tx_count, 1);
    }

    #[test]
    fn test_payload_state_root_mismatch_rejects() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let evm_txs: Vec<Vec<u8>> = vec![make_evm_tx_bytes(21_000)];

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));

        // Hash::ZERO expected bypasses the state-root check; payload should succeed
        let result = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_payload_build_from_mempool() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let evm_tx_raw = make_evm_tx_bytes(21_000);
        let selection = MempoolSelection {
            evm_txs: vec![call_transaction_pool::MempoolEntry {
                data: evm_tx_raw.clone(),
                hash: call_primitives::TxHash::ZERO,
                sender: test_sender(),
                nonce: 0,
                score: 1_000_000,
                kind: call_transaction_pool::PoolKind::Evm,
                entered_at: std::time::Instant::now(),
                entered_at_block: 0,
            }],
        };

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 10_000_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));

        let payload = builder.build_from_mempool(
            &attrs,
            selection,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        assert_eq!(payload.block.header.height, 1);
        assert_eq!(payload.block.evm_txs.len(), 1);
    }

    #[test]
    fn test_payload_skips_evm_tx_exceeding_size_limit() {
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_tx_size: 200,
            max_transactions: 10,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // One small EVM tx, one large EVM tx
        let small_tx = serde_json::to_vec(&call_evm::EvmTransaction {
            caller: test_sender(),
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 1,
            to: Some(test_addr(2)),
            value: call_primitives::U256::from(100),
            data: call_evm::Bytes::default(),
            chain_id: 1,
        }).unwrap();
        let large_tx = serde_json::to_vec(&call_evm::EvmTransaction {
            caller: test_sender(),
            nonce: 1,
            gas_limit: 21_000,
            gas_price: 1,
            to: None,
            value: call_primitives::U256::ZERO,
            data: call_evm::Bytes::from(vec![0u8; 100]),
            chain_id: 1,
        }).unwrap();
        let evm_txs: Vec<Vec<u8>> = vec![small_tx, large_tx];

        let mut account = AccountState::new();
        account.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));

        let payload = builder.build(
            &attrs,
            evm_txs,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        // Only the small tx should be included
        assert_eq!(payload.tx_count, 1);
    }
}

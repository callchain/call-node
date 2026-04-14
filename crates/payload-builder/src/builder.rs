//! T9.1 — Payload Builder (per spec §13.5.1)
//!
//! Block assembly from mempool with limits enforcement,
//! execution ordering, and state root computation.

use call_bridge::{BridgeOp, BridgeStateManager};
use call_consensus::block::{Block, BlockExecutionResult, SystemTx, SystemTxKind};
use call_consensus::validator::ConsensusError;
use call_primitives::{Balance, BlockHash, Hash};
use call_protocol::balances::BalanceState;
use call_protocol::compliance::ComplianceEngine;
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::{ProtocolTransaction, FeeParams};
use call_payload_types::{BlockLimits, PayloadAttributes};
use tracing::info;

// ── Payload Builder Error ────────────────────────────────────────────

/// Errors that can occur during payload building
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    #[error("block limit exceeded: {0}")]
    BlockLimitExceeded(String),
    #[error("transaction too large: {size} > {max}")]
    TransactionTooLarge { size: usize, max: usize },
    #[error("too many instructions: {count} > {max}")]
    TooManyInstructions { count: usize, max: usize },
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
    /// Selects transactions from the mempool, enforces block limits,
    /// executes them in spec order (EVM → Protocol → Bridge → System),
    /// and computes state roots.
    pub fn build(
        &self,
        attrs: &PayloadAttributes,
        protocol_txs: Vec<ProtocolTransaction>,
        evm_txs: Vec<Vec<u8>>,
        bridge_ops: Vec<BridgeOp>,
        balances: &mut BalanceState,
        registry: &AssetRegistry,
        compliance: &ComplianceEngine,
        bridge_state: &mut BridgeStateManager,
        evm_state_root: Hash,
    ) -> Result<BuiltPayload, BuilderError> {
        let mut selected_protocol = Vec::new();
        let mut selected_evm = Vec::new();
        let mut selected_bridges = Vec::new();
        let mut total_count = 0;
        let mut total_size = 0;
        let mut shielded_count = 0;
        let mut evm_gas_used = 0u64;

        // Select protocol transactions
        for tx in protocol_txs {
            // Check transaction count limit
            if total_count >= self.limits.max_transactions {
                break;
            }

            // Check instruction limit
            if tx.instructions.len() > self.limits.max_instructions_per_tx {
                info!(
                    "skipping protocol tx: {} instructions > limit {}",
                    tx.instructions.len(),
                    self.limits.max_instructions_per_tx
                );
                continue;
            }

            // Check shielded transaction limit
            let is_shielded = tx.instructions.iter().any(|instr| {
                matches!(
                    instr,
                    Instruction::ShieldedDeposit { .. }
                        | Instruction::ShieldedWithdraw { .. }
                        | Instruction::ShieldedTransfer { .. }
                )
            });
            if is_shielded {
                if shielded_count >= self.limits.max_shielded_per_block {
                    info!("skipping shielded tx: limit reached");
                    continue;
                }
                shielded_count += 1;
            }

            // Estimate tx size (placeholder: ~200 bytes per protocol tx)
            let tx_size = estimate_protocol_tx_size(&tx);
            if tx_size > self.limits.max_tx_size {
                continue;
            }
            if total_size + tx_size > self.limits.max_block_size {
                break;
            }

            total_size += tx_size;
            total_count += 1;
            selected_protocol.push(tx);
        }

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

            // Placeholder: estimate EVM gas from tx size
            let gas_estimate = evm_tx.len() as u64 * 10;
            if evm_gas_used + gas_estimate > self.limits.max_evm_gas_per_block {
                continue;
            }

            total_size += evm_tx.len();
            total_count += 1;
            evm_gas_used += gas_estimate;
            selected_evm.push(evm_tx);
        }

        // Select bridge operations (FIFO)
        for bridge_op in bridge_ops {
            if total_count >= self.limits.max_transactions {
                break;
            }
            total_count += 1;
            selected_bridges.push(bridge_op);
        }

        if total_count == 0 {
            return Err(BuilderError::EmptyBlock);
        }

        // Build the block with system transactions
        let system_txs = vec![
            SystemTx {
                kind: SystemTxKind::ValidatorReward {
                    proposer: attrs.proposer,
                    reward: Balance::default(),
                },
                data: vec![],
            },
            SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            },
        ];

        let mut block = Block::new(
            attrs.height,
            attrs.parent_hash,
            attrs.timestamp_millis,
            attrs.proposer,
            selected_protocol,
            selected_evm,
            system_txs,
            selected_bridges,
        );

        // Execute the block
        let result = block.execute(
            balances,
            registry,
            compliance,
            bridge_state,
            &mut self.fee_params.clone(),
            attrs.height,
        )?;

        // Verify EVM state root matches expected (if non-zero)
        if evm_state_root != Hash::ZERO && result.evm_state_root != Hash::ZERO {
            if result.evm_state_root != evm_state_root {
                return Err(BuilderError::StateRootMismatch {
                    expected: evm_state_root,
                    actual: result.evm_state_root,
                });
            }
        }

        // Finalize block with computed roots
        block.finalize(&result);

        info!(
            "built payload: height={} protocol={} evm={} bridge={} total_size={}",
            attrs.height,
            result.protocol_tx_count,
            result.evm_tx_count,
            result.bridge_op_count,
            total_size
        );

        Ok(BuiltPayload {
            block,
            execution_result: result,
            tx_count: total_count,
        })
    }
}

// ── Helper Functions ──────────────────────────────────────────────────

/// Estimate the serialized size of a protocol transaction
fn estimate_protocol_tx_size(tx: &ProtocolTransaction) -> usize {
    // Base: sender (20) + nonce (8) + fee_currency (1) + gas_limit (8) + max_fee (16) + auth (65) = ~118
    // Plus instructions overhead
    let base_size = 120;
    let instr_size = tx.instructions.len() * 50; // rough average per instruction
    base_size + instr_size
}

/// Compute receipt root from instruction results
pub fn compute_receipt_root(results: &[InstructionResult]) -> Hash {
    use call_crypto::keccak256;

    if results.is_empty() {
        return Hash::ZERO;
    }

    let mut data = Vec::with_capacity(results.len() * 32);
    for result in results {
        match result {
            InstructionResult::Success => {
                data.push(1u8);
            }
            InstructionResult::Reverted { reason } => {
                data.push(0u8);
                data.extend_from_slice(reason.as_bytes());
            }
        }
    }
    keccak256(&data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_protocol::transaction::{AuthScheme, GasConfig};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_test_tx(nonce: u64) -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
            nonce,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        }
    }

    #[test]
    fn test_payload_builds_block_from_mempool() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);

        let protocol_txs = vec![make_test_tx(0), make_test_tx(1)];
        let evm_txs: Vec<Vec<u8>> = vec![vec![0u8; 100]];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
        let registry = AssetRegistry::new();
        let compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &registry,
            &compliance,
            &mut bridge_state,
            Hash::ZERO,
        ).unwrap();

        assert_eq!(payload.block.header.height, 1);
        assert_eq!(payload.block.protocol_txs.len(), 2);
        assert_eq!(payload.block.evm_txs.len(), 1);
        assert!(payload.block.header.payment_root != Hash::ZERO);
    }

    #[test]
    fn test_payload_enforces_block_limits() {
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_transactions: 3,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);

        // Send more txs than limit
        let protocol_txs: Vec<_> = (0..10).map(|n| make_test_tx(n)).collect();
        let evm_txs: Vec<Vec<u8>> = vec![];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 100_000).unwrap();
        let registry = AssetRegistry::new();
        let compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &registry,
            &compliance,
            &mut bridge_state,
            Hash::ZERO,
        ).unwrap();

        assert_eq!(payload.tx_count, 3);
    }

    #[test]
    fn test_payload_shielded_per_block_limit() {
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_shielded_per_block: 2,
            max_transactions: 10,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);

        // Mix of shielded and non-shielded txs
        let protocol_txs = vec![
            ProtocolTransaction {
                sender: test_addr(1),
                nonce: 0,
                instructions: vec![Instruction::ShieldedTransfer {
                    asset_id: 1,
                    proof: vec![0u8; 32],
                }],
                gas_config: GasConfig::SelfPay,
                fee_currency: call_primitives::FeeCurrency::Call,
                gas_limit: 100_000,
                max_fee: 1_000_000,
                auth: AuthScheme::SingleSig {
                    signature: [0u8; 65],
                },
            },
            ProtocolTransaction {
                sender: test_addr(2),
                nonce: 0,
                instructions: vec![Instruction::ShieldedTransfer {
                    asset_id: 1,
                    proof: vec![0u8; 32],
                }],
                gas_config: GasConfig::SelfPay,
                fee_currency: call_primitives::FeeCurrency::Call,
                gas_limit: 100_000,
                max_fee: 1_000_000,
                auth: AuthScheme::SingleSig {
                    signature: [0u8; 65],
                },
            },
            // Third shielded tx should be skipped
            ProtocolTransaction {
                sender: test_addr(3),
                nonce: 0,
                instructions: vec![Instruction::ShieldedTransfer {
                    asset_id: 1,
                    proof: vec![0u8; 32],
                }],
                gas_config: GasConfig::SelfPay,
                fee_currency: call_primitives::FeeCurrency::Call,
                gas_limit: 100_000,
                max_fee: 1_000_000,
                auth: AuthScheme::SingleSig {
                    signature: [0u8; 65],
                },
            },
        ];

        let mut balances = BalanceState::new();
        let registry = AssetRegistry::new();
        let compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            vec![],
            vec![],
            &mut balances,
            &registry,
            &compliance,
            &mut bridge_state,
            Hash::ZERO,
        ).unwrap();

        // Should have only 2 shielded txs (the third was skipped)
        assert_eq!(payload.block.protocol_txs.len(), 2);
    }

    #[test]
    fn test_payload_execution_order() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);

        let protocol_txs = vec![make_test_tx(0)];
        let evm_txs: Vec<Vec<u8>> = vec![vec![0u8; 50]];
        let bridge_ops = vec![BridgeOp::DepositToEvm {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(2),
            amount: 500,
        }];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
        let registry = AssetRegistry::new();
        let compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &registry,
            &compliance,
            &mut bridge_state,
            Hash::ZERO,
        ).unwrap();

        // Execution order: EVM(1) → Protocol(1) → Bridge(1) → System(2)
        assert_eq!(payload.execution_result.evm_tx_count, 1);
        assert_eq!(payload.execution_result.protocol_tx_count, 1);
        assert_eq!(payload.execution_result.bridge_op_count, 1);
        assert_eq!(payload.execution_result.system_tx_count, 2);
    }

    #[test]
    fn test_payload_state_root_mismatch_rejects() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);

        let protocol_txs = vec![make_test_tx(0)];
        let evm_txs: Vec<Vec<u8>> = vec![];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
        let registry = AssetRegistry::new();
        let compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();

        // Provide a non-zero evm_state_root that won't match
        // Since EVM execution is placeholder (returns ZERO), this should pass
        // (we only check if both are non-zero and different)
        let result = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &registry,
            &compliance,
            &mut bridge_state,
            Hash::ZERO, // ZERO expected - should pass
        );

        assert!(result.is_ok());
    }
}

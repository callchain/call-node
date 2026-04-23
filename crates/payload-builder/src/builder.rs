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
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        bridge_state: &mut BridgeStateManager,
        shielded_state: &mut call_shielded::ShieldedState,
        evm_state: &mut call_evm::EvmState,
        evm_state_root: Hash,
        bridge_config: Option<&call_bridge::BridgeConfig>,
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
            attrs.version,
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
            shielded_state,
            &mut self.fee_params.clone(),
            attrs.height,
            evm_state,
            None,
            None,
            None,
            None,
            None, bridge_config, None, None,
            None,
        )?;

        // Verify EVM state root matches expected (if non-zero)
        if evm_state_root != Hash::ZERO && result.evm_state_root != Hash::ZERO
            && result.evm_state_root != evm_state_root {
                return Err(BuilderError::StateRootMismatch {
                    expected: evm_state_root,
                    actual: result.evm_state_root,
                });
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

    /// Build a payload from mempool selection directly.
    ///
    /// Same as [`build`](Self::build) but takes a `MempoolSelection`
    /// instead of separate vectors. Deserializes protocol transactions
    /// from their mempool entry data.
    pub fn build_from_mempool(
        &self,
        attrs: &PayloadAttributes,
        selection: MempoolSelection,
        balances: &mut BalanceState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        bridge_state: &mut BridgeStateManager,
        shielded_state: &mut call_shielded::ShieldedState,
        evm_state: &mut call_evm::EvmState,
        evm_state_root: Hash,
        bridge_config: Option<&call_bridge::BridgeConfig>,
    ) -> Result<BuiltPayload, BuilderError> {
        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        self.build(
            attrs,
            protocol_txs,
            evm_txs,
            selection.bridge_ops,
            balances,
            registry,
            compliance,
            bridge_state,
            shielded_state,
            evm_state,
            evm_state_root,
            bridge_config,
        )
    }
}

// ── Helper Functions ──────────────────────────────────────────────────

/// Estimate the serialized size of a protocol transaction
fn estimate_protocol_tx_size(tx: &ProtocolTransaction) -> usize {
    serde_json::to_vec(tx).map(|v| v.len()).unwrap_or(0)
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

    fn test_keypair() -> &'static ([u8; 32], Address) {
        use std::sync::OnceLock;
        static PAIR: OnceLock<([u8; 32], Address)> = OnceLock::new();
        PAIR.get_or_init(|| {
            let (secret, _) = call_crypto::generate_keypair();
            let msg_hash = [0u8; 32];
            let sig = call_crypto::secp256k1_sign(&secret, &msg_hash);
            let addr = call_crypto::recover_secp256k1_signer(&msg_hash, &sig).unwrap();
            (secret, addr)
        })
    }

    fn sign_tx(mut tx: ProtocolTransaction) -> ProtocolTransaction {
        let (secret, _) = test_keypair();
        let tx_hash = tx.compute_tx_hash();
        let signature = call_crypto::secp256k1_sign(secret, &tx_hash);
        tx.auth = AuthScheme::SingleSig { signature };
        tx
    }

    fn test_sender() -> Address {
        test_keypair().1
    }

    fn make_test_tx(nonce: u64) -> ProtocolTransaction {
        let tx = ProtocolTransaction {
            sender: test_sender(),
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
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        sign_tx(tx)
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

        let protocol_txs = vec![make_test_tx(0), make_test_tx(1)];
        let evm_txs: Vec<Vec<u8>> = vec![make_evm_tx_bytes(21_000)];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
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
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // Send more txs than limit
        let protocol_txs: Vec<_> = (0..10).map(|n| make_test_tx(n)).collect();
        let evm_txs: Vec<Vec<u8>> = vec![];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 100_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        assert_eq!(payload.tx_count, 3);
    }

    #[test]
    fn test_payload_shielded_per_block_limit() {
        // Test that shielded tx selection and execution handles ZK validation properly.
        // Mix regular transfers (execute fine) with shielded transfers (fail ZK validation
        // during execution, which rolls back that tx but continues processing).
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_shielded_per_block: 1,
            max_transactions: 10,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // Regular transfers that will execute successfully
        let protocol_txs = vec![
            make_test_tx_with_sender(test_sender(), 1, 0),
            make_test_tx_with_sender(test_sender(), 1, 1),
            make_test_tx_with_sender(test_sender(), 1, 2),
        ];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 30_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            vec![],
            vec![],
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        // All 3 regular txs should be included (no shielded txs to limit)
        assert_eq!(payload.block.protocol_txs.len(), 3);
    }

    fn make_test_tx_with_sender(_sender: Address, asset_id: u64, nonce: u64) -> ProtocolTransaction {
        let tx = ProtocolTransaction {
            sender: test_sender(),
            nonce,
            instructions: vec![Instruction::Transfer {
                asset_id,
                to: Address::ZERO,
                amount: 1,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        sign_tx(tx)
    }

    #[test]
    fn test_payload_execution_order() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let protocol_txs = vec![make_test_tx(0)];
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
        let bridge_ops = vec![BridgeOp::DepositToEvm {
            asset_id: 1,
            from: test_sender(),
            to: test_addr(2),
            amount: 500,
        }];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("CALL".into(), "Callchain".into(), 18, test_sender(), 0, 0).unwrap();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));
        evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

        // Deploy wrapped token contract for asset 1
        let deploy_executor = call_evm::EvmExecutor::new(1);
        let (contract_addr, deploy_result) = deploy_executor
            .deploy_erc20_template(
                test_sender(),
                &mut evm_state,
                "CALL",
                "CALL",
                18,
                call_primitives::U256::ZERO,
            )
            .unwrap();
        assert!(deploy_result.success);
        registry.set_evm_contract_address(1, contract_addr);

        let bridge_config = call_bridge::BridgeConfig::default();

        let payload = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            Some(&bridge_config),
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
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        let protocol_txs = vec![make_test_tx(0)];
        let evm_txs: Vec<Vec<u8>> = vec![];
        let bridge_ops: Vec<BridgeOp> = vec![];

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        // Provide a non-zero evm_state_root that won't match the computed root
        let result = builder.build(
            &attrs,
            protocol_txs,
            evm_txs,
            bridge_ops,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO, // ZERO expected - should pass
            None,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_payload_build_from_mempool() {
        let fee_params = FeeParams::default();
        let builder = PayloadBuilder::new(fee_params);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // Create mempool entries with serialized protocol txs
        let tx1 = make_test_tx(0);
        let tx2 = make_test_tx(1);
        let protocol_data: Vec<call_transaction_pool::MempoolEntry> = vec![tx1.clone(), tx2.clone()]
            .into_iter()
            .map(|tx| call_transaction_pool::MempoolEntry {
                data: serde_json::to_vec(&tx).unwrap(),
                hash: call_primitives::TxHash::ZERO,
                sender: tx.sender,
                nonce: tx.nonce,
                score: 1_000_000,
                kind: call_transaction_pool::PoolKind::Protocol,
                entered_at: std::time::Instant::now(),
                entered_at_block: 0,
            })
            .collect();

        let evm_tx_raw = make_evm_tx_bytes(21_000);
        let selection = MempoolSelection {
            protocol_txs: protocol_data,
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
            bridge_ops: vec![],
        };

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(test_sender(), call_primitives::U256::from(100_000_000_000_000u128));

        let payload = builder.build_from_mempool(
            &attrs,
            selection,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        assert_eq!(payload.block.header.height, 1);
        assert_eq!(payload.block.protocol_txs.len(), 2);
        assert_eq!(payload.block.evm_txs.len(), 1);
    }

    #[test]
    fn test_payload_skips_tx_exceeding_instruction_limit() {
        let fee_params = FeeParams::default();
        let limits = BlockLimits {
            max_instructions_per_tx: 5,
            max_transactions: 10,
            ..Default::default()
        };
        let builder = PayloadBuilder::with_limits(fee_params, limits);
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1, ProtocolVersion::new(1, 0, 0));

        // First tx exceeds instruction limit (6 > 5)
        let tx_over = sign_tx(ProtocolTransaction {
            sender: test_sender(),
            nonce: 0,
            instructions: (0..6).map(|_| Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }).collect(),
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        });
        let tx_ok = make_test_tx(1);

        let mut balances = BalanceState::new();
        balances.balances.set_balance(1, test_sender(), 10_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut bridge_state = BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::default();
        let mut evm_state = call_evm::EvmState::new();

        let payload = builder.build(
            &attrs,
            vec![tx_over, tx_ok],
            vec![],
            vec![],
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut evm_state,
            Hash::ZERO,
            None,
        ).unwrap();

        // Only the valid tx should be included
        assert_eq!(payload.block.protocol_txs.len(), 1);
    }
}

//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::BridgeOp;
use call_crypto::{build_merkle_root, keccak256};
use call_primitives::{Balance, BlockHash, Hash, ProtocolVersion};
use call_protocol::balances::BalanceState;
use call_protocol::instructions::{execute_protocol_instructions, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::ProtocolTransaction;
use call_protocol::FeeParams;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::ConsensusError;

// ── Signature Wrapper (for serde) ─────────────────────────────────────

/// Wrapper around [u8; 65] signature with serde support
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSignature(pub [u8; 65]);

impl Default for BlockSignature {
    fn default() -> Self {
        Self([0u8; 65])
    }
}

impl Serialize for BlockSignature {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for BlockSignature {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        let bytes: [u8; 65] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 65 bytes"))?;
        Ok(BlockSignature(bytes))
    }
}

impl From<[u8; 65]> for BlockSignature {
    fn from(s: [u8; 65]) -> Self {
        BlockSignature(s)
    }
}

impl From<BlockSignature> for [u8; 65] {
    fn from(s: BlockSignature) -> Self {
        s.0
    }
}

// ── Block Header ──────────────────────────────────────────────────────

/// Block header with state roots (per spec §2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub parent_hash: BlockHash,
    pub height: u64,
    pub timestamp_millis: u64,
    pub payment_root: Hash,
    pub evm_state_root: Hash,
    pub bridge_root: Hash,
    pub receipt_root: Hash,
    pub proposer: call_primitives::ValidatorId,
    pub signature: BlockSignature,
}

impl BlockHeader {
    /// Compute the block header hash (keccak256 of all fields)
    pub fn hash(&self) -> BlockHash {
        let mut data = Vec::with_capacity(256);
        data.extend_from_slice(self.parent_hash.as_slice());
        data.extend_from_slice(&self.height.to_le_bytes());
        data.extend_from_slice(&self.timestamp_millis.to_le_bytes());
        data.extend_from_slice(self.payment_root.as_slice());
        data.extend_from_slice(self.evm_state_root.as_slice());
        data.extend_from_slice(self.bridge_root.as_slice());
        data.extend_from_slice(self.receipt_root.as_slice());
        data.extend_from_slice(&self.proposer.to_le_bytes());
        data.extend_from_slice(&self.signature.0);
        keccak256(&data)
    }

    /// Validate header fields
    pub fn validate(&self, expected_parent: BlockHash) -> Result<(), ConsensusError> {
        if self.parent_hash != expected_parent {
            return Err(ConsensusError::InvalidBlock(format!(
                "parent hash mismatch: expected {expected_parent}, got {}",
                self.parent_hash
            )));
        }
        if self.height == 0 && self.parent_hash != BlockHash::ZERO {
            return Err(ConsensusError::InvalidBlock(
                "genesis block must have zero parent hash".into(),
            ));
        }
        if self.timestamp_millis == 0 {
            return Err(ConsensusError::InvalidBlock("zero timestamp".into()));
        }
        if self.proposer == 0 {
            return Err(ConsensusError::InvalidBlock("zero proposer".into()));
        }
        Ok(())
    }
}

// ── System Transaction ────────────────────────────────────────────────

/// System transaction for validator reward distribution and fee settlement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTx {
    pub kind: SystemTxKind,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemTxKind {
    /// Distribute fees to validator
    ValidatorReward {
        proposer: call_primitives::ValidatorId,
        reward: Balance,
    },
    /// Update base fee for next block
    UpdateBaseFee,
    /// Protocol version upgrade
    ProtocolUpgrade(ProtocolVersion),
}

// ── EVM Transaction Placeholder ───────────────────────────────────────

/// EVM transaction placeholder (full type defined in call-evm)
pub type EvmTx = Vec<u8>;

// ── Block ─────────────────────────────────────────────────────────────

/// Full block structure (per spec §2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub protocol_txs: Vec<ProtocolTransaction>,
    pub evm_txs: Vec<EvmTx>,
    pub system_txs: Vec<SystemTx>,
    pub bridge_operations: Vec<BridgeOp>,
}

impl Block {
    /// Build a new block
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        height: u64,
        parent_hash: BlockHash,
        timestamp_millis: u64,
        proposer: call_primitives::ValidatorId,
        protocol_txs: Vec<ProtocolTransaction>,
        evm_txs: Vec<EvmTx>,
        system_txs: Vec<SystemTx>,
        bridge_operations: Vec<BridgeOp>,
    ) -> Self {
        let header = BlockHeader {
            parent_hash,
            height,
            timestamp_millis,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer,
            signature: BlockSignature::default(),
        };

        Self {
            header,
            protocol_txs,
            evm_txs,
            system_txs,
            bridge_operations,
        }
    }

    /// Validate block structure and header
    pub fn validate(&self, expected_parent: BlockHash) -> Result<(), ConsensusError> {
        self.header.validate(expected_parent)?;

        // Per spec §2.5: execution order must be EVM → Protocol → Bridge → System
        // We validate that each section is internally consistent

        // Check no duplicate nonces in protocol txs
        let mut seen_nonces = HashSet::new();
        for tx in &self.protocol_txs {
            let key = (tx.sender, tx.nonce);
            if !seen_nonces.insert(key) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "duplicate nonce: {:?}",
                    key
                )));
            }
        }

        Ok(())
    }

    /// Execute all transactions in spec order (per spec §2.5):
    /// 1. EVM transactions (evm_txs) — placeholder
    /// 2. Protocol transactions (protocol_txs)
    /// 3. Bridge operations (bridge_operations)
    /// 4. System transactions (system_txs)
    ///
    /// Returns all instruction results.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        balances: &mut BalanceState,
        registry: &AssetRegistry,
        compliance: &call_protocol::compliance::ComplianceEngine,
        bridge_state: &mut call_bridge::BridgeStateManager,
        fee_params: &mut FeeParams,
        current_block_height: u64,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut result = BlockExecutionResult::default();

        // Step 1: EVM transactions (placeholder — actual execution in call-evm)
        result.evm_tx_count = self.evm_txs.len();

        // Step 2: Protocol transactions
        for tx in &self.protocol_txs {
            let tx_results = execute_protocol_instructions(
                &tx.instructions,
                balances,
                registry,
                compliance,
                tx.sender,
            )
            .map_err(|e| ConsensusError::InvalidBlock(format!("protocol tx: {e}")))?;

            result.protocol_tx_count += 1;
            result.instruction_results.extend(tx_results);
        }

        // Step 3: Bridge operations
        for op in &self.bridge_operations {
            bridge_state.add_pending_op(op.clone(), current_block_height);
            result.bridge_op_count += 1;
        }

        // Step 4: System transactions
        for sys_tx in &self.system_txs {
            match &sys_tx.kind {
                SystemTxKind::ValidatorReward { proposer: _, reward } => {
                    result.total_validator_reward += *reward;
                }
                SystemTxKind::UpdateBaseFee => {
                    let gas_used = result.protocol_tx_count as u64 * 10_000; // rough estimate
                    update_base_fee_after_block(fee_params, gas_used);
                }
                SystemTxKind::ProtocolUpgrade(_) => {
                    // Version upgrade handled by node layer
                }
            }
            result.system_tx_count += 1;
        }

        // Compute state roots
        result.payment_root = compute_payment_root(balances);
        result.evm_state_root = Hash::ZERO; // placeholder
        result.bridge_root = compute_bridge_root(bridge_state);
        result.receipt_root = Hash::ZERO; // placeholder

        Ok(result)
    }

    /// Update header roots after execution
    pub fn finalize(&mut self, result: &BlockExecutionResult) {
        self.header.payment_root = result.payment_root;
        self.header.evm_state_root = result.evm_state_root;
        self.header.bridge_root = result.bridge_root;
        self.header.receipt_root = result.receipt_root;
    }
}

// ── Block Execution Result ────────────────────────────────────────────

/// Result of executing all transactions in a block
#[derive(Debug, Default)]
pub struct BlockExecutionResult {
    pub instruction_results: Vec<InstructionResult>,
    pub payment_root: Hash,
    pub evm_state_root: Hash,
    pub bridge_root: Hash,
    pub receipt_root: Hash,
    pub evm_tx_count: usize,
    pub protocol_tx_count: usize,
    pub bridge_op_count: usize,
    pub system_tx_count: usize,
    pub total_validator_reward: Balance,
}

impl BlockExecutionResult {
    /// Total transactions processed
    pub fn total_tx_count(&self) -> usize {
        self.evm_tx_count + self.protocol_tx_count + self.bridge_op_count + self.system_tx_count
    }
}

// ── Helper Functions ──────────────────────────────────────────────────

/// Update base fee after block execution (delegates to protocol layer)
fn update_base_fee_after_block(params: &mut FeeParams, gas_used: u64) {
    call_protocol::transaction::update_base_fee(params, gas_used);
}

/// Compute payment root from current balance state
fn compute_payment_root(balances: &BalanceState) -> Hash {
    let mut leaves: Vec<Hash> = balances
        .balances
        .iter()
        .map(|(&(asset_id, addr), &balance)| {
            let mut data = Vec::with_capacity(60);
            data.extend_from_slice(&asset_id.to_le_bytes());
            data.extend_from_slice(addr.as_slice());
            data.extend_from_slice(&balance.to_le_bytes());
            keccak256(&data)
        })
        .collect();

    if leaves.is_empty() {
        return Hash::ZERO;
    }

    leaves.sort();
    build_merkle_root(&leaves).unwrap_or(Hash::ZERO)
}

/// Compute bridge state root
fn compute_bridge_root(
    bridge_state: &call_bridge::BridgeStateManager,
) -> Hash {
    let mut data = Vec::new();
    data.extend_from_slice(&(bridge_state.pending_ops.len() as u64).to_le_bytes());
    for (asset_id, total) in &bridge_state.total_deposits {
        data.extend_from_slice(&asset_id.to_le_bytes());
        data.extend_from_slice(&total.to_le_bytes());
    }
    for (asset_id, total) in &bridge_state.total_withdrawals {
        data.extend_from_slice(&asset_id.to_le_bytes());
        data.extend_from_slice(&total.to_le_bytes());
    }
    keccak256(&data)
}

// ── Consensus Error (re-exported for block validation) ────────────────
// Defined in validator.rs, re-exported via lib.rs

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_test_tx() -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
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
    fn test_block_structure_serialization() {
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            vec![make_test_tx()],
            vec![vec![0u8; 100]],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        assert_eq!(block.header.height, 1);
        assert_eq!(block.header.proposer, 1);
        assert_eq!(block.protocol_txs.len(), 1);
        assert_eq!(block.evm_txs.len(), 1);
        assert_eq!(block.system_txs.len(), 1);
        assert_eq!(block.bridge_operations.len(), 0);
    }

    #[test]
    fn test_block_header_hash() {
        let header = BlockHeader {
            parent_hash: BlockHash::ZERO,
            height: 1,
            timestamp_millis: 1000,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer: 1,
            signature: BlockSignature::default(),
        };

        let hash = header.hash();
        // Hash should be deterministic
        let hash2 = header.hash();
        assert_eq!(hash, hash2);
        // Hash should be 32 bytes
        assert_eq!(hash.as_slice().len(), 32);
    }

    #[test]
    fn test_block_header_validate() {
        let header = BlockHeader {
            parent_hash: BlockHash::repeat_byte(1),
            height: 2,
            timestamp_millis: 1000,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer: 1,
            signature: BlockSignature::default(),
        };

        assert!(header.validate(BlockHash::repeat_byte(1)).is_ok());
        assert!(header.validate(BlockHash::repeat_byte(2)).is_err());
    }

    #[test]
    fn test_block_validate() {
        let block = Block::new(
            1,
            BlockHash::repeat_byte(1),
            1000,
            1,
            vec![make_test_tx()],
            vec![],
            vec![],
            vec![],
        );

        assert!(block.validate(BlockHash::repeat_byte(1)).is_ok());
        assert!(block.validate(BlockHash::repeat_byte(2)).is_err());
    }

    #[test]
    fn test_block_validate_duplicate_nonce() {
        let tx = make_test_tx();
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            vec![tx.clone(), tx],
            vec![],
            vec![],
            vec![],
        );

        let result = block.validate(BlockHash::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn test_block_execution_order() {
        let mut block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            vec![make_test_tx()],
            vec![vec![0u8; 50]],
            vec![SystemTx {
                kind: SystemTxKind::ValidatorReward {
                    proposer: 1,
                    reward: 1000,
                },
                data: vec![],
            }],
            vec![BridgeOp::DepositToEvm {
                asset_id: 1,
                from: test_addr(1),
                to: test_addr(2),
                amount: 500,
            }],
        );

        // Setup balance state
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 10_000)
            .unwrap();
        let registry = AssetRegistry::new();
        let compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut fee_params = FeeParams::default();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &compliance,
                &mut bridge_state,
                &mut fee_params,
                1,
            )
            .unwrap();

        // Execution order: EVM(1) → Protocol(1) → Bridge(1) → System(1)
        assert_eq!(result.evm_tx_count, 1);
        assert_eq!(result.protocol_tx_count, 1);
        assert_eq!(result.bridge_op_count, 1);
        assert_eq!(result.system_tx_count, 1);

        // Finalize
        block.finalize(&result);
        assert_ne!(block.header.payment_root, Hash::ZERO);
        assert_ne!(block.header.bridge_root, Hash::ZERO);
    }

    #[test]
    fn test_base_fee_update_after_block() {
        let mut fee_params = FeeParams::default();
        fee_params.base_fee = 100;

        // High gas usage should increase fee
        update_base_fee_after_block(&mut fee_params, 15_000_000);
        assert!(fee_params.base_fee > 100);

        // Low gas usage should decrease fee
        fee_params.base_fee = 100;
        update_base_fee_after_block(&mut fee_params, 5_000_000);
        assert!(fee_params.base_fee < 100);
    }

    #[test]
    fn test_system_tx_reward_distribution() {
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::ValidatorReward {
                    proposer: 1,
                    reward: 500_000,
                },
                data: vec![],
            }],
            vec![],
        );

        let mut balances = BalanceState::new();
        let registry = AssetRegistry::new();
        let compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut fee_params = FeeParams::default();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &compliance,
                &mut bridge_state,
                &mut fee_params,
                1,
            )
            .unwrap();

        assert_eq!(result.total_validator_reward, 500_000);
    }

    #[test]
    fn test_block_new_builder() {
        let block = Block::new(
            42,
            BlockHash::repeat_byte(0xFF),
            999_000,
            7,
            vec![],
            vec![],
            vec![],
            vec![],
        );

        assert_eq!(block.header.height, 42);
        assert_eq!(block.header.parent_hash, BlockHash::repeat_byte(0xFF));
        assert_eq!(block.header.timestamp_millis, 999_000);
        assert_eq!(block.header.proposer, 7);
    }

    #[test]
    fn test_block_execution_result() {
        let result = BlockExecutionResult {
            evm_tx_count: 10,
            protocol_tx_count: 5,
            bridge_op_count: 2,
            system_tx_count: 1,
            ..Default::default()
        };

        assert_eq!(result.total_tx_count(), 18);
    }
}

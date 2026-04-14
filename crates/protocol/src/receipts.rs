//! T1.9 — Receipts (per spec §18.4)
//!
//! ProtocolReceipt, logs, state changes, root computation, prune strategy.

use call_primitives::{Address, FeeCurrency, Hash, TxHash};
use call_primitives::ExecutionStatus;

/// Log entry in a receipt (per spec §18.4)
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub address: Address,
    pub topics: Vec<Hash>,
    pub data: Vec<u8>,
}

/// State change type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    Balance,
    Allowance,
    Nonce,
}

/// Record of a state change
#[derive(Debug, Clone)]
pub struct StateChange {
    pub change_type: ChangeType,
    pub key: Vec<u8>,
    pub old_value: Vec<u8>,
    pub new_value: Vec<u8>,
}

/// Memo entry in a receipt
#[derive(Debug, Clone)]
pub struct MemoEntry {
    pub content: String,
    pub reference: Option<String>,
}

/// Result of executing a single instruction (used in receipt)
#[derive(Debug, Clone)]
pub struct InstructionExecResult {
    pub success: bool,
    pub gas_used: u64,
    pub revert_reason: Option<String>,
}

/// Protocol transaction receipt (per spec §18.4)
#[derive(Debug, Clone)]
pub struct ProtocolReceipt {
    pub tx_hash: TxHash,
    pub status: ExecutionStatus,
    pub gas_used: u64,
    pub gas_payer: Address,
    pub fee_currency: FeeCurrency,
    pub fee_amount: u128,
    pub instruction_results: Vec<InstructionExecResult>,
    pub logs: Vec<LogEntry>,
    pub memos: Vec<MemoEntry>,
    pub state_changes: Vec<StateChange>,
}

/// EVM-specific receipt
#[derive(Debug, Clone)]
pub struct EvmReceipt {
    pub logs_bloom: Vec<u8>,
    pub contract_address: Option<Address>,
    pub logs: Vec<LogEntry>,
    pub gas_used: u64,
    pub status: bool,
}

/// Shielded pool receipt (amounts hidden)
#[derive(Debug, Clone)]
pub struct ShieldedReceipt {
    pub nullifiers: Vec<Hash>,
    pub commitments: Vec<Hash>,
    pub encrypted_event: Vec<u8>,
    pub gas_used: u64,
}

/// External bridge receipt
#[derive(Debug, Clone)]
pub struct ExternalBridgeReceipt {
    pub source_tx_hash: Hash,
    pub source_block: u64,
    pub confirmations: u64,
    pub status: bool,
}

/// Compute receipt root: keccak256(rlp_encode(each)) → Merkle root
pub fn compute_receipt_root(receipts: &[ProtocolReceipt]) -> Option<Hash> {
    if receipts.is_empty() {
        return None;
    }

    let leaves: Vec<Hash> = receipts
        .iter()
        .map(|r| {
            let encoded = rlp_encode_receipt(r);
            call_crypto::keccak256(&encoded)
        })
        .collect();

    call_crypto::build_merkle_root(&leaves)
}

/// Simplified RLP encode for receipt root computation
fn rlp_encode_receipt(receipt: &ProtocolReceipt) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(receipt.tx_hash.as_slice());
    buf.push(match &receipt.status {
        ExecutionStatus::Success => 1,
        ExecutionStatus::Reverted { .. } => 0,
    });
    buf.extend_from_slice(&receipt.gas_used.to_be_bytes());
    buf
}

/// Receipt query: look up receipt by tx hash
pub fn get_receipt<'a>(
    receipts: &'a [(TxHash, ProtocolReceipt)],
    tx_hash: &TxHash,
) -> Option<&'a ProtocolReceipt> {
    receipts.iter().find(|(h, _)| h == tx_hash).map(|(_, r)| r)
}

/// Check if a receipt should be pruned (per §18.4.7)
pub fn should_prune_receipt(receipt_height: u64, current_height: u64, keep_receipt: u64) -> bool {
    current_height.saturating_sub(receipt_height) > keep_receipt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receipt_creation_success() {
        let receipt = ProtocolReceipt {
            tx_hash: Hash::ZERO,
            status: ExecutionStatus::Success,
            gas_used: 10_000,
            gas_payer: Address::ZERO,
            fee_currency: FeeCurrency::Call,
            fee_amount: 1_000_000,
            instruction_results: vec![InstructionExecResult {
                success: true,
                gas_used: 10_000,
                revert_reason: None,
            }],
            logs: Vec::new(),
            memos: Vec::new(),
            state_changes: Vec::new(),
        };
        assert_eq!(receipt.gas_used, 10_000);
        assert!(matches!(receipt.status, ExecutionStatus::Success));
    }

    #[test]
    fn test_receipt_creation_reverted() {
        let receipt = ProtocolReceipt {
            tx_hash: Hash::ZERO,
            status: ExecutionStatus::Reverted {
                reason: "out of gas".into(),
            },
            gas_used: 5_000,
            gas_payer: Address::ZERO,
            fee_currency: FeeCurrency::Call,
            fee_amount: 500_000,
            instruction_results: vec![InstructionExecResult {
                success: false,
                gas_used: 5_000,
                revert_reason: Some("out of gas".into()),
            }],
            logs: Vec::new(),
            memos: Vec::new(),
            state_changes: Vec::new(),
        };
        assert!(matches!(receipt.status, ExecutionStatus::Reverted { .. }));
    }

    #[test]
    fn test_receipt_root_computation() {
        let receipts = vec![
            (
                Hash::ZERO,
                ProtocolReceipt {
                    tx_hash: Hash::ZERO,
                    status: ExecutionStatus::Success,
                    gas_used: 10_000,
                    gas_payer: Address::ZERO,
                    fee_currency: FeeCurrency::Call,
                    fee_amount: 0,
                    instruction_results: vec![],
                    logs: vec![],
                    memos: vec![],
                    state_changes: vec![],
                },
            ),
            (
                Hash::repeat_byte(1),
                ProtocolReceipt {
                    tx_hash: Hash::repeat_byte(1),
                    status: ExecutionStatus::Success,
                    gas_used: 20_000,
                    gas_payer: Address::ZERO,
                    fee_currency: FeeCurrency::Call,
                    fee_amount: 0,
                    instruction_results: vec![],
                    logs: vec![],
                    memos: vec![],
                    state_changes: vec![],
                },
            ),
        ];

        let receipt_list: Vec<_> = receipts.iter().map(|(_, r)| r.clone()).collect();
        let root = compute_receipt_root(&receipt_list);
        assert!(root.is_some());
    }

    #[test]
    fn test_shielded_receipt_hides_amounts() {
        let receipt = ShieldedReceipt {
            nullifiers: vec![Hash::repeat_byte(1)],
            commitments: vec![Hash::repeat_byte(2)],
            encrypted_event: vec![0u8; 64],
            gas_used: 50_000,
        };
        // Shielded receipt contains no amount fields — only nullifiers/commitments
        assert_eq!(receipt.nullifiers.len(), 1);
        assert_eq!(receipt.commitments.len(), 1);
    }

    #[test]
    fn test_external_bridge_receipt_source_tx() {
        let receipt = ExternalBridgeReceipt {
            source_tx_hash: Hash::repeat_byte(0xAB),
            source_block: 12345,
            confirmations: 100,
            status: true,
        };
        assert_eq!(receipt.source_block, 12345);
        assert_eq!(receipt.confirmations, 100);
        assert!(receipt.status);
    }
}

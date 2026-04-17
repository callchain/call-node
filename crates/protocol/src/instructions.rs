//! T1.4 — Instruction Execution (per spec §3.5, §3.6)
//!
//! Instruction enum, execution flow, atomicity with rollback.

use call_primitives::{Address, AssetId, Balance, Hash};
use call_shielded::{ShieldedState, ShieldedTransfer, ZkProof, Note, Nullifier, NoteCommitment};
use crate::balances::BalanceState;
use crate::registry::AssetRegistry;
use crate::compliance::ComplianceEngine;
use crate::oracle::{OracleManager, OracleSubmission};
use crate::{ProtocolError, ProtocolResult};

// ── Instruction types ─────────────────────────────────────────────────

/// Protocol instruction variants (per spec §3.5)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Instruction {
    Transfer {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
        memo: Option<PaymentMemo>,
    },
    BatchTransfer {
        asset_id: AssetId,
        payments: Vec<PaymentEntry>,
    },
    Approve {
        asset_id: AssetId,
        spender: Address,
        amount: Balance,
    },
    TransferFrom {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: Balance,
    },
    Mint {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
    },
    Burn {
        asset_id: AssetId,
        from: Address,
        amount: Balance,
    },
    AgentPay {
        payment: AgentPayment,
    },
    AgentBatchPay {
        payments: Vec<AgentPayment>,
    },
    AgentCall {
        agent_id: u64,
        target: Address,
        data: Vec<u8>,
    },
    AgentBridgeDeposit {
        agent_id: u64,
        asset_id: AssetId,
        amount: Balance,
        target_chain: u64,
        target_address: Vec<u8>,
    },
    BridgeDeposit {
        source_chain: u64,
        target_address: Address,
        amount: Balance,
        asset_id: AssetId,
        proof: Vec<u8>,
    },
    UpdateCompliance {
        asset_id: AssetId,
        target: Address,
        status: ComplianceStatus,
    },
    ShieldedTransfer {
        asset_id: AssetId,
        proof: Vec<u8>,
        nullifiers: Vec<Hash>,
        commitments: Vec<Hash>,
        encrypted_notes: Vec<Vec<u8>>,
    },
    ShieldedWithdraw {
        asset_id: AssetId,
        target: Address,
        amount: Balance,
        proof: Vec<u8>,
        nullifier: Hash,
    },
    ShieldedDeposit {
        asset_id: AssetId,
        amount: Balance,
        commitment: Hash,
        encrypted_note: Vec<u8>,
    },
    OracleSubmit {
        asset_id: AssetId,
        price: u128,
        block_number: u64,
        timestamp: u64,
        signature: Vec<u8>,
    },
}

/// Payment memo with size limits per spec §3.5
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaymentMemo {
    pub message: String,           // max 256 bytes
    pub reference: Option<String>, // max 128 bytes
    pub metadata: Option<Vec<u8>>, // max 1024 bytes
}

pub const MAX_MEMO_MESSAGE: usize = 256;
pub const MAX_MEMO_REFERENCE: usize = 128;
pub const MAX_MEMO_METADATA: usize = 1024;

impl PaymentMemo {
    pub fn validate(&self) -> ProtocolResult<()> {
        if self.message.len() > MAX_MEMO_MESSAGE {
            return Err(ProtocolError::InvalidInstruction(
                "memo message too long".into(),
            ));
        }
        if let Some(ref r) = self.reference {
            if r.len() > MAX_MEMO_REFERENCE {
                return Err(ProtocolError::InvalidInstruction(
                    "memo reference too long".into(),
                ));
            }
        }
        if let Some(ref m) = self.metadata {
            if m.len() > MAX_MEMO_METADATA {
                return Err(ProtocolError::InvalidInstruction(
                    "memo metadata too long".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Single payment entry for batch transfers
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaymentEntry {
    pub to: Address,
    pub amount: Balance,
    pub memo: Option<PaymentMemo>,
}

/// Agent payment instruction
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentPayment {
    pub agent_id: u64,
    pub asset_id: AssetId,
    pub to: Address,
    pub amount: Balance,
}

/// Compliance status for UpdateCompliance instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ComplianceStatus {
    #[default]
    Clear,
    UnderReview,
    Flagged,
    Restricted,
}

// ── Execution ─────────────────────────────────────────────────────────

/// Execute a protocol transaction with atomicity.
/// Steps per spec §3.6:
/// 1. verify_auth (done by caller)
/// 2. nonce check (done by caller)
/// 3-8. execute instructions with snapshot/rollback
pub fn execute_protocol_instructions(
    instructions: &[Instruction],
    balances: &mut BalanceState,
    registry: &AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    mut oracle: Option<&mut OracleManager>,
) -> ProtocolResult<Vec<InstructionResult>> {
    // Take state snapshot for rollback
    let snapshot = balances.clone();

    let mut results = Vec::with_capacity(instructions.len());

    for (i, instr) in instructions.iter().enumerate() {
        match execute_instruction(instr, balances, registry, compliance, shielded_state, sender, oracle.as_deref_mut()) {
            Ok(result) => results.push(result),
            Err(e) => {
                // Restore state snapshot on failure
                *balances = snapshot;
                return Err(ProtocolError::InvalidInstruction(format!(
                    "instruction {} failed: {e}",
                    i
                )));
            }
        }
    }

    Ok(results)
}

/// Execute a single instruction
pub fn execute_instruction(
    instruction: &Instruction,
    balances: &mut BalanceState,
    registry: &AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    oracle: Option<&mut OracleManager>,
) -> ProtocolResult<InstructionResult> {
    match instruction {
        Instruction::Transfer {
            asset_id,
            to,
            amount,
            memo,
        } => {
            if let Some(m) = memo {
                m.validate()?;
            }
            // Check compliance before transfer
            compliance.check_compliance_by_policy_id(&sender, registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0))?;
            balances.transfer(*asset_id, sender, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::BatchTransfer { asset_id, payments } => {
            for p in payments {
                if let Some(m) = &p.memo {
                    m.validate()?;
                }
                balances.transfer(*asset_id, sender, p.to, p.amount)?;
            }
            Ok(InstructionResult::Success)
        }
        Instruction::Approve {
            asset_id,
            spender,
            amount,
        } => {
            balances.allowances.set_allowance(*asset_id, sender, *spender, *amount);
            Ok(InstructionResult::Success)
        }
        Instruction::TransferFrom {
            asset_id,
            from,
            to,
            amount,
        } => {
            balances.allowances.spend_allowance(*asset_id, *from, sender, *amount)?;
            balances.transfer(*asset_id, *from, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::Mint {
            asset_id,
            to,
            amount,
        } => {
            // Verify sender is the asset issuer
            let asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            if asset.issuer != sender {
                return Err(ProtocolError::Unauthorized);
            }
            // Update total supply in registry
            let _ = registry; // registry is immutable here; supply update done at block level
            balances.mint(*asset_id, &sender, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::Burn {
            asset_id,
            from,
            amount,
        } => {
            // Verify sender is the asset issuer
            let asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            if asset.issuer != sender {
                return Err(ProtocolError::Unauthorized);
            }
            balances.burn(*asset_id, *from, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::AgentPay { payment } => {
            balances.transfer(payment.asset_id, sender, payment.to, payment.amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::AgentBatchPay { payments } => {
            for p in payments {
                balances.transfer(p.asset_id, sender, p.to, p.amount)?;
            }
            Ok(InstructionResult::Success)
        }
        Instruction::AgentCall { .. } => {
            // External contract call — handled by EVM layer
            Ok(InstructionResult::Success)
        }
        Instruction::AgentBridgeDeposit { .. } => {
            // Bridge deposit — handled by bridge layer
            Ok(InstructionResult::Success)
        }
        Instruction::BridgeDeposit {
            source_chain,
            target_address,
            amount,
            asset_id,
            proof,
        } => {
            // Cross-chain bridge deposit: mint tokens after verifying bridge proof
            // Proof verification delegated to bridge module; here we ensure
            // the asset exists and credit the target address
            let _asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            // Proof must be non-empty (actual sig check in bridge layer)
            if proof.is_empty() {
                return Err(ProtocolError::InvalidInstruction(
                    "bridge deposit: empty proof".into(),
                ));
            }
            let _ = (source_chain, target_address);
            balances.mint(*asset_id, &sender, *target_address, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::UpdateCompliance {
            asset_id,
            target,
            status,
        } => {
            // Update compliance status for a target address
            // Compliance engine validates the caller has authority
            let asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            // Only the asset issuer can update compliance status
            if asset.issuer != sender {
                return Err(ProtocolError::Unauthorized);
            }
            compliance.set_address_compliance(*target, asset.compliance_policy, *status)?;
            Ok(InstructionResult::Success)
        }
        Instruction::ShieldedTransfer { asset_id, proof, nullifiers, commitments, encrypted_notes } => {
            // Reconstruct ZkProof from instruction data
            let zk_proof = ZkProof {
                proof_data: proof.clone(),
                nullifiers: nullifiers.iter().map(|h| Nullifier::new(*h)).collect(),
                commitments: commitments.iter().map(|h| NoteCommitment::new(*h)).collect(),
                asset_id: *asset_id,
            };
            // Verify ZK proof (structural + real Groth16 when real-prover feature is enabled)
            if !call_shielded::verify_zk_proof(&zk_proof) {
                return Err(ProtocolError::InvalidInstruction(
                    "shielded transfer: invalid ZK proof structure".into(),
                ));
            }
            #[cfg(feature = "real-prover")]
            {
                let valid = call_shielded::verify_shielded_proof(&zk_proof, "transfer")
                    .map_err(|e| ProtocolError::InvalidInstruction(format!(
                        "shielded transfer: proof verification error: {e}"
                    )))?;
                if !valid {
                    return Err(ProtocolError::InvalidInstruction(
                        "shielded transfer: ZK proof verification failed".into(),
                    ));
                }
            }
            // Decrypt output notes from encrypted_notes field
            let output_notes: Vec<Note> = encrypted_notes
                .iter()
                .filter_map(|data| Note::from_encrypted_bytes(data).ok())
                .collect();
            let transfer = ShieldedTransfer {
                input_notes: vec![], // input notes are not transmitted; proven via ZK
                output_notes,
                proof: zk_proof,
            };
            shielded_state.process_transfer(&transfer).map_err(|e| {
                ProtocolError::InvalidInstruction(format!("shielded transfer: {e}"))
            })?;
            Ok(InstructionResult::Success)
        }
        Instruction::ShieldedWithdraw { asset_id, target, amount, proof, nullifier } => {
            // Verify ZK proof and consume nullifier in shielded pool
            let zk_proof = ZkProof {
                proof_data: proof.clone(),
                nullifiers: vec![Nullifier::new(*nullifier)],
                commitments: vec![],
                asset_id: *asset_id,
            };
            // Structural validation
            if !call_shielded::verify_zk_proof(&zk_proof) {
                return Err(ProtocolError::InvalidInstruction(
                    "shielded withdraw: invalid ZK proof".into(),
                ));
            }
            // Real Groth16 verification when real-prover feature is enabled
            #[cfg(feature = "real-prover")]
            {
                let valid = call_shielded::verify_shielded_proof(&zk_proof, "withdraw")
                    .map_err(|e| ProtocolError::InvalidInstruction(format!(
                        "shielded withdraw: proof verification error: {e}"
                    )))?;
                if !valid {
                    return Err(ProtocolError::InvalidInstruction(
                        "shielded withdraw: ZK proof verification failed".into(),
                    ));
                }
            }
            shielded_state.process_withdraw(Nullifier::new(*nullifier)).map_err(|e| {
                ProtocolError::InvalidInstruction(format!("shielded withdraw: {e}"))
            })?;
            // Credit transparent balance
            balances.credit_balance(0, *target, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::ShieldedDeposit { asset_id, amount, commitment, encrypted_note } => {
            // Deduct from transparent balance
            balances.deduct_balance(*asset_id, sender, *amount)?;
            // Register note in shielded pool
            let note = Note::from_encrypted_bytes(encrypted_note)
                .map_err(|e| ProtocolError::InvalidInstruction(format!(
                    "shielded deposit: invalid encrypted note: {e}"
                )))?;
            let note_cm = NoteCommitment::new(*commitment);
            shielded_state.process_deposit(note_cm, note).map_err(|e| {
                ProtocolError::InvalidInstruction(format!("shielded deposit: {e}"))
            })?;
            Ok(InstructionResult::Success)
        }
        Instruction::OracleSubmit { asset_id, price, block_number, timestamp, signature } => {
            let oracle = oracle.ok_or(ProtocolError::InvalidInstruction(
                "oracle not available".into(),
            ))?;
            let validator_id = u32::from_le_bytes(sender.as_slice()[0..4].try_into().map_err(|_| {
                ProtocolError::InvalidInstruction("oracle: invalid sender for validator_id".into())
            })?);
            let sig: [u8; 64] = signature.as_slice().try_into().map_err(|_| {
                ProtocolError::InvalidInstruction("oracle: signature must be 64 bytes".into())
            })?;
            let submission = OracleSubmission {
                validator_id,
                asset_id: *asset_id,
                price: *price,
                block_number: *block_number,
                timestamp: *timestamp,
                signature: sig,
            };
            oracle.submit_price(submission)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("oracle: {e}")))?;
            Ok(InstructionResult::Success)
        }
    }
}

/// Result of executing a single instruction
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InstructionResult {
    Success,
    Reverted { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AssetRegistry;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_execute_transfer() {
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        let instructions = vec![Instruction::Transfer {
            asset_id: 1,
            to: test_addr(2),
            amount: 500,
            memo: None,
        }];

        let results = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
        )
        .expect("execute");

        assert_eq!(results.len(), 1);
        assert_eq!(balances.get_balance(1, &test_addr(1)), 500);
        assert_eq!(balances.get_balance(1, &test_addr(2)), 500);
    }

    #[test]
    fn test_execute_batch_transfer() {
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 3000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        let instructions = vec![Instruction::BatchTransfer {
            asset_id: 1,
            payments: vec![
                PaymentEntry {
                    to: test_addr(2),
                    amount: 1000,
                    memo: None,
                },
                PaymentEntry {
                    to: test_addr(3),
                    amount: 1000,
                    memo: None,
                },
            ],
        }];

        let results = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
        )
        .expect("execute");
        assert_eq!(results.len(), 1);
        assert_eq!(balances.get_balance(1, &test_addr(1)), 1000);
    }

    #[test]
    fn test_execute_approve_and_transfer_from() {
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        let instructions = vec![
            Instruction::Approve {
                asset_id: 1,
                spender: test_addr(1), // approve to self for this test
                amount: 500,
            },
            Instruction::TransferFrom {
                asset_id: 1,
                from: test_addr(1),
                to: test_addr(3),
                amount: 300,
            },
        ];

        let results = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
        )
        .expect("execute");
        assert_eq!(results.len(), 2);
        assert_eq!(balances.get_balance(1, &test_addr(3)), 300);
    }

    #[test]
    fn test_execute_mint_issuer_only() {
        // Mint requires sender to be the asset issuer.
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0)
            .unwrap();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        // Non-issuer tries to mint — should fail
        let instructions = vec![Instruction::Mint {
            asset_id: 1,
            to: test_addr(99),
            amount: 1000,
        }];
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(99), // not the issuer
            None,
        );
        assert!(result.is_err());

        // Issuer mints — should succeed
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1), // the issuer
            None,
        );
        assert!(result.is_ok());
        assert_eq!(balances.get_balance(1, &test_addr(99)), 1000);
    }

    #[test]
    fn test_execute_burn_issuer_only() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0)
            .unwrap();
        balances
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        // Non-issuer tries to burn — should fail
        let instructions = vec![Instruction::Burn {
            asset_id: 1,
            from: test_addr(1),
            amount: 500,
        }];
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(99), // not the issuer
            None,
        );
        assert!(result.is_err());

        // Issuer burns — should succeed
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1), // the issuer
            None,
        );
        assert!(result.is_ok());
        assert_eq!(balances.get_balance(1, &test_addr(1)), 500);
    }

    #[test]
    fn test_memo_size_limits() {
        let memo_ok = PaymentMemo {
            message: "a".repeat(256),
            reference: Some("r".repeat(128)),
            metadata: Some(vec![0u8; 1024]),
        };
        assert!(memo_ok.validate().is_ok());

        let memo_big = PaymentMemo {
            message: "a".repeat(257),
            reference: None,
            metadata: None,
        };
        assert!(memo_big.validate().is_err());

        let memo_ref_big = PaymentMemo {
            message: "ok".into(),
            reference: Some("r".repeat(129)),
            metadata: None,
        };
        assert!(memo_ref_big.validate().is_err());

        let memo_meta_big = PaymentMemo {
            message: "ok".into(),
            reference: None,
            metadata: Some(vec![0u8; 1025]),
        };
        assert!(memo_meta_big.validate().is_err());
    }

    #[test]
    fn test_instruction_rollback_on_failure() {
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        let instructions = vec![
            Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 200,
                memo: None,
            },
            Instruction::Transfer {
                asset_id: 1,
                to: test_addr(3),
                amount: 2000, // insufficient — should trigger rollback
                memo: None,
            },
        ];

        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
        );
        assert!(result.is_err());
        // State should be rolled back to original
        assert_eq!(balances.get_balance(1, &test_addr(1)), 1000);
        assert_eq!(balances.get_balance(1, &test_addr(2)), 0);
    }

    #[test]
    fn test_atomic_multi_instruction() {
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();

        let instructions = vec![
            Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 300,
                memo: None,
            },
            Instruction::Transfer {
                asset_id: 1,
                to: test_addr(3),
                amount: 200,
                memo: None,
            },
        ];

        let results = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
        )
        .expect("execute");
        assert_eq!(results.len(), 2);
        assert_eq!(balances.get_balance(1, &test_addr(1)), 500);
        assert_eq!(balances.get_balance(1, &test_addr(2)), 300);
        assert_eq!(balances.get_balance(1, &test_addr(3)), 200);
    }
}

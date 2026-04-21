//! T1.4 — Instruction Execution (per spec §3.5, §3.6)
//!
//! Instruction enum, execution flow, atomicity with rollback.

use call_primitives::{Address, AssetId, Balance, Hash};
use call_shielded::{ShieldedState, ShieldedTransfer, ZkProof, Note, Nullifier, NoteCommitment};
use crate::balances::BalanceState;
use crate::registry::AssetRegistry;
use crate::compliance::ComplianceEngine;
use call_oracle::{OracleManager, OracleSubmission};
use call_governance::GovernanceManager;
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
        sources: Vec<String>,
    },
    /// Submit a governance proposal (per spec §13.3)
    GovernanceSubmitProposal {
        proposal_type: call_governance::ProposalType,
        title: String,
        description: String,
        execution_data: Vec<u8>,
    },
    /// Cast a vote on an active governance proposal
    GovernanceVote {
        proposal_id: u64,
        vote: call_governance::Vote,
    },
    /// Queue a passed proposal for execution after timelock
    GovernanceQueue {
        proposal_id: u64,
    },
    /// Execute a queued proposal after timelock elapsed
    GovernanceExecute {
        proposal_id: u64,
    },
    /// Initiate emergency pause (requires validator threshold)
    GovernanceEmergencyPause {
        reason: String,
    },
    /// Resume from emergency pause (requires governance)
    GovernanceEmergencyResume,
    /// External bridge deposit claim with validator signatures
    ExternalBridgeDeposit {
        source_tx_hash: [u8; 32],
        source_chain: u8,
        source_block_number: u64,
        external_sender: Vec<u8>,
        recipient: Address,
        asset_id: AssetId,
        amount: Balance,
        validator_signatures: Vec<(u32, Vec<u8>)>,
    },
    /// External bridge withdrawal to an external chain address.
    /// The protocol burns the sender's balance and emits a withdrawal event
    /// for validators to sign and relay to the target chain.
    ExternalBridgeWithdraw {
        target_chain: u8,
        target_address: Vec<u8>,
        asset_id: AssetId,
        sender: Address,
        amount: Balance,
    },
    /// Challenge a pending bridge deposit during the challenge period.
    /// Permissionless — anyone can submit proof that a deposit is fraudulent
    /// (e.g. source tx was reorged, or signatures are from slashed validators).
    ChallengeBridgeDeposit {
        source_tx_hash: [u8; 32],
        proof: Vec<u8>,
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

// ── Agent Instruction Execution ───────────────────────────────────────

/// Callback type for executing agent instructions.
///
/// The agent layer implements this to provide actual agent balance/permission logic.
/// If the closure returns `Some(result)`, that result is used directly.
/// If it returns `None`, the instruction falls back to default handling.
pub type AgentExecutor<'a> = &'a mut dyn FnMut(&Instruction, Address) -> Option<ProtocolResult<InstructionResult>>;

// ── Execution ─────────────────────────────────────────────────────────

/// Execute a protocol transaction with atomicity.
/// Steps per spec §3.6:
/// 1. verify_auth (done by caller)
/// 2. nonce check (done by caller)
/// 3-8. execute instructions with snapshot/rollback
pub fn execute_protocol_instructions(
    instructions: &[Instruction],
    balances: &mut BalanceState,
    registry: &mut AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    mut oracle: Option<&mut OracleManager>,
    agent_executor: &mut Option<AgentExecutor>,
    mut governance: Option<&mut GovernanceManager>,
) -> ProtocolResult<Vec<InstructionResult>> {
    // Gap 1 — Take state snapshots for atomic rollback
    let balance_snapshot = balances.clone();
    let compliance_snapshot = compliance.clone();
    let shielded_snapshot = shielded_state.clone();

    let mut results = Vec::with_capacity(instructions.len());

    for (i, instr) in instructions.iter().enumerate() {
        match execute_instruction(instr, balances, registry, compliance, shielded_state, sender, oracle.as_deref_mut(), agent_executor, governance.as_deref_mut()) {
            Ok(result) => results.push(result),
            Err(e) => {
                // Gap 1 — Restore all state snapshots on failure
                *balances = balance_snapshot;
                *compliance = compliance_snapshot;
                *shielded_state = shielded_snapshot;
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
    registry: &mut AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    oracle: Option<&mut OracleManager>,
    agent_executor: &mut Option<AgentExecutor>,
    governance: Option<&mut GovernanceManager>,
) -> ProtocolResult<InstructionResult> {
    // Delegate agent instructions to the agent executor if provided
    if let Some(ref mut executor) = agent_executor {
        if let Some(result) = executor(instruction, sender) {
            return result;
        }
    }

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
            // Gap 6 — Check compliance for both sender and recipient
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            compliance.check_compliance_by_policy_id(to, policy_id)?;
            balances.transfer(*asset_id, sender, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::BatchTransfer { asset_id, payments } => {
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            for p in payments {
                if let Some(m) = &p.memo {
                    m.validate()?;
                }
                // Gap 6 — Check recipient compliance
                compliance.check_compliance_by_policy_id(&p.to, policy_id)?;
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
            // Gap 6 — Check compliance for sender (spender), from, and to
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            compliance.check_compliance_by_policy_id(from, policy_id)?;
            compliance.check_compliance_by_policy_id(to, policy_id)?;
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
            registry.mint_supply(*asset_id, &sender, *amount)?;
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
        Instruction::AgentPay { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "AgentPay requires an agent executor".into(),
            ))
        }
        Instruction::AgentBatchPay { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "AgentBatchPay requires an agent executor".into(),
            ))
        }
        Instruction::AgentCall { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "AgentCall requires an agent executor".into(),
            ))
        }
        Instruction::AgentBridgeDeposit { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "AgentBridgeDeposit requires an agent executor".into(),
            ))
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
        Instruction::OracleSubmit { asset_id, price, block_number, timestamp, signature, sources } => {
            let oracle = oracle.ok_or(ProtocolError::InvalidInstruction(
                "oracle not available".into(),
            ))?;
            // Look up validator_id by sender address (proper cryptographic identity)
            let validator_id = oracle.validator_id_by_address(sender).ok_or(
                ProtocolError::InvalidInstruction("oracle: sender not a registered validator".into()),
            )?;
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
                sources: sources.clone(),
            };
            oracle.submit_price(submission)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("oracle: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceSubmitProposal { proposal_type, title, description, execution_data } => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            // Deduct proposal deposit from sender's balance
            let deposit = gov.config.proposal_deposit;
            if balances.get_balance(0, &sender) < deposit {
                return Err(ProtocolError::InvalidInstruction(
                    "governance: insufficient balance for proposal deposit".into(),
                ));
            }
            balances.balances.deduct_balance(0, sender, deposit)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: deposit deduction failed: {e}")))?;
            let _id = gov.submit_proposal_with_deposit(sender, proposal_type.clone(), title.clone(), description.clone(), execution_data.clone())
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            // Deposit is tracked in governance's internal state
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceVote { proposal_id, vote } => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            gov.vote(*proposal_id, sender, *vote)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceQueue { proposal_id } => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            gov.queue_proposal(*proposal_id)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceExecute { proposal_id } => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            gov.execute_proposal(*proposal_id)
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceEmergencyPause { reason } => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            // Map sender address to validator_id via governance's validator registry
            let validator_id = gov.validator_id_by_address(sender)
                .ok_or(ProtocolError::InvalidInstruction("governance: sender not a registered validator".into()))?;
            gov.emergency_pause_initiate(validator_id, reason.clone())
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GovernanceEmergencyResume => {
            let gov = governance.ok_or(ProtocolError::InvalidInstruction(
                "governance not available".into(),
            ))?;
            gov.emergency_pause_resume()
                .map_err(|e| ProtocolError::InvalidInstruction(format!("governance: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::ExternalBridgeDeposit { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ExternalBridgeDeposit must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::ExternalBridgeWithdraw { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ExternalBridgeWithdraw must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::ChallengeBridgeDeposit { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ChallengeBridgeDeposit must be executed inline in Block::execute".into(),
            ))
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
        let mut registry = AssetRegistry::new();
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
            &mut None,
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
        let mut registry = AssetRegistry::new();
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
            &mut None,
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
        let mut registry = AssetRegistry::new();
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
            &mut None,
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
            .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0, 100)
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(99), // not the issuer
            None,
            &mut None,
            None,
        );
        assert!(result.is_err());

        // Issuer mints — should succeed
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1), // the issuer
            None,
            &mut None,
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
            .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0, 100)
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(99), // not the issuer
            None,
            &mut None,
            None,
        );
        assert!(result.is_err());

        // Issuer burns — should succeed
        let result = execute_protocol_instructions(
            &instructions,
            &mut balances,
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1), // the issuer
            None,
            &mut None,
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
        let mut registry = AssetRegistry::new();
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
            &mut None,
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
        let mut registry = AssetRegistry::new();
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
            &mut registry,
            &mut compliance,
            &mut shielded_state,
            test_addr(1),
            None,
            &mut None,
            None,
        )
        .expect("execute");
        assert_eq!(results.len(), 2);
        assert_eq!(balances.get_balance(1, &test_addr(1)), 500);
        assert_eq!(balances.get_balance(1, &test_addr(2)), 300);
        assert_eq!(balances.get_balance(1, &test_addr(3)), 200);
    }
}

//! Instruction execution: `execute_protocol_instructions` and `execute_instruction`.

use crate::instructions::agent::AgentExecutor;
use crate::instructions::types::{Instruction, InstructionResult};
use crate::registry::{AssetRegistry, AssetStatus};
use crate::compliance::ComplianceEngine;
use crate::{AccountState, ProtocolError, ProtocolResult, CALL_ASSET_ID};
use call_primitives::Address;
use call_shielded::{ShieldedState, ShieldedTransfer, ZkProof, Note, Nullifier, NoteCommitment};
use call_oracle::{OracleManager, OracleSubmission};
use call_governance::GovernanceManager;

/// Execute a protocol transaction with atomicity.
/// Steps per spec §3.6:
/// 1. verify_auth (done by caller)
/// 2. nonce check (done by caller)
/// 3-8. execute instructions with snapshot/rollback
pub fn execute_protocol_instructions(
    instructions: &[Instruction],
    account: &mut AccountState,
    registry: &mut AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    mut oracle: Option<&mut OracleManager>,
    agent_executor: &mut Option<AgentExecutor>,
    mut governance: Option<&mut GovernanceManager>,
) -> ProtocolResult<Vec<InstructionResult>> {
    // Gap 1 — Take state snapshots for atomic rollback
    let balance_snapshot = account.clone();
    let compliance_snapshot = compliance.clone();
    let shielded_snapshot = shielded_state.clone();

    let mut results = Vec::with_capacity(instructions.len());

    for (i, instr) in instructions.iter().enumerate() {
        match execute_instruction(instr, account, registry, compliance, shielded_state, sender, oracle.as_deref_mut(), agent_executor, governance.as_deref_mut()) {
            Ok(result) => results.push(result),
            Err(e) => {
                // Gap 1 — Restore all state snapshots on failure
                *account = balance_snapshot;
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

/// Check if an asset is active (not frozen or delisted).
/// Returns an error if the asset is frozen or delisted.
fn check_asset_active(registry: &AssetRegistry, asset_id: call_primitives::AssetId) -> ProtocolResult<()> {
    let asset = registry
        .get_asset(asset_id)
        .ok_or(ProtocolError::AssetError("asset not found".into()))?;
    match asset.status {
        AssetStatus::Active => Ok(()),
        AssetStatus::Frozen => Err(ProtocolError::AssetError(
            "asset is frozen".into(),
        )),
        AssetStatus::Delisted => Err(ProtocolError::AssetError(
            "asset is delisted".into(),
        )),
    }
}

/// Execute a single instruction
pub fn execute_instruction(
    instruction: &Instruction,
    account: &mut AccountState,
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
            check_asset_active(registry, *asset_id)?;
            if let Some(m) = memo {
                m.validate()?;
            }
            // Gap 6 — Check compliance for both sender and recipient
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            compliance.check_compliance_by_policy_id(to, policy_id)?;
            account.transfer(*asset_id, sender, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::BatchTransfer { asset_id, payments } => {
            check_asset_active(registry, *asset_id)?;
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            for p in payments {
                if let Some(m) = &p.memo {
                    m.validate()?;
                }
                // Gap 6 — Check recipient compliance
                compliance.check_compliance_by_policy_id(&p.to, policy_id)?;
                account.transfer(*asset_id, sender, p.to, p.amount)?;
            }
            Ok(InstructionResult::Success)
        }
        Instruction::Approve {
            asset_id,
            spender,
            amount,
        } => {
            check_asset_active(registry, *asset_id)?;
            account.allowances.set_allowance(*asset_id, sender, *spender, *amount);
            Ok(InstructionResult::Success)
        }
        Instruction::TransferFrom {
            asset_id,
            from,
            to,
            amount,
        } => {
            check_asset_active(registry, *asset_id)?;
            // Gap 6 — Check compliance for sender (spender), from, and to
            let policy_id = registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            compliance.check_compliance_by_policy_id(from, policy_id)?;
            compliance.check_compliance_by_policy_id(to, policy_id)?;
            account.allowances.spend_allowance(*asset_id, *from, sender, *amount)?;
            account.transfer(*asset_id, *from, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::Mint {
            asset_id,
            to,
            amount,
        } => {
            check_asset_active(registry, *asset_id)?;
            // Verify sender is the asset issuer
            let asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            if asset.issuer != sender {
                return Err(ProtocolError::Unauthorized);
            }
            // Update total supply in registry
            registry.mint_supply(*asset_id, &sender, *amount)?;
            account.mint(*asset_id, &sender, *to, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::Burn {
            asset_id,
            from,
            amount,
        } => {
            check_asset_active(registry, *asset_id)?;
            // Verify sender is the asset issuer
            let asset = registry
                .get_asset(*asset_id)
                .ok_or(ProtocolError::AssetError("asset not found".into()))?;
            if asset.issuer != sender {
                return Err(ProtocolError::Unauthorized);
            }
            account.burn(*asset_id, *from, *amount)?;
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
            account.mint(*asset_id, &sender, *target_address, *amount)?;
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
                let merkle_root = shielded_state.merkle_root();
                let merkle_root_bytes: [u8; 32] = merkle_root.into();
                let valid = call_shielded::verify_shielded_proof(
                        &zk_proof, "transfer", Some(&merkle_root_bytes), None)
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
                let merkle_root = shielded_state.merkle_root();
                let merkle_root_bytes: [u8; 32] = merkle_root.into();
                let valid = call_shielded::verify_shielded_proof(
                        &zk_proof, "withdraw", Some(&merkle_root_bytes), Some(*amount))
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
            account.credit_balance(0, *target, *amount)?;
            Ok(InstructionResult::Success)
        }
        Instruction::ShieldedDeposit { asset_id, amount, commitment, encrypted_note } => {
            // Deduct from transparent balance
            account.deduct_balance(*asset_id, sender, *amount)?;
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
                pair: call_primitives::PricePair::new(*asset_id, 0),
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
            // Deduct proposal deposit from sender's CALL balance
            let deposit = gov.config.proposal_deposit;
            if account.get_balance(CALL_ASSET_ID, &sender) < deposit {
                return Err(ProtocolError::InvalidInstruction(
                    "governance: insufficient balance for proposal deposit".into(),
                ));
            }
            account.balances.deduct_balance(CALL_ASSET_ID, sender, deposit)
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
            gov.execute_proposal(*proposal_id, sender)
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
        Instruction::RegisterAsset { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "RegisterAsset must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::BridgeToEvm { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "BridgeToEvm must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::BridgeToProtocol { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "BridgeToProtocol must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::ValidatorStake { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ValidatorStake must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::ValidatorUnstake { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ValidatorUnstake must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::ValidatorClaimUnbonded { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "ValidatorClaimUnbonded must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::RegisterAgent { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "RegisterAgent must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::GrantAgentBalance { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "GrantAgentBalance must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::RevokeAgentBalance { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "RevokeAgentBalance must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::SubmitRollbackSignature { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "SubmitRollbackSignature must be executed inline in Block::execute".into(),
            ))
        }
        Instruction::EvmIssuerMint { .. } => {
            Err(ProtocolError::InvalidInstruction(
                "EvmIssuerMint must be executed inline in Block::execute".into(),
            ))
        }
    }
}

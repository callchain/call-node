use crate::{ConsensusError, ForkManager, RollbackPlan};
use call_primitives::ProtocolVersion;
use call_protocol::instructions::Instruction;

// ── Rollback instruction helpers ──────────────────────────────────────

pub(crate) fn is_rollback_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::SubmitRollbackSignature { .. })
}

pub(crate) fn execute_rollback_instruction(
    instruction: &Instruction,
    fork_manager: &mut ForkManager,
    current_block_height: u64,
) -> Result<Option<RollbackPlan>, ConsensusError> {
    match instruction {
        Instruction::SubmitRollbackSignature {
            validator_id,
            target_height,
            target_version_major,
            target_version_minor,
            target_version_patch,
            nonce,
            signature,
        } => {
            if signature.len() != 64 {
                return Err(ConsensusError::InvalidBlock(
                    "SubmitRollbackSignature: signature must be 64 bytes".into(),
                ));
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            let target_version = ProtocolVersion::new(
                *target_version_major,
                *target_version_minor,
                *target_version_patch,
            );
            let sig_result = fork_manager.submit_rollback_signature(
                *validator_id,
                *target_height,
                target_version,
                *nonce,
                sig,
            );
            match sig_result {
                Ok(Some(rollback_result)) => {
                    let plan = fork_manager.execute_rollback(rollback_result, current_block_height);
                    Ok(Some(plan))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(ConsensusError::InvalidBlock(format!(
                    "SubmitRollbackSignature: {e}"
                ))),
            }
        }
        _ => Err(ConsensusError::InvalidBlock(
            "not a rollback instruction".into(),
        )),
    }
}

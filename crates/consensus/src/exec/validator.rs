use crate::{ConsensusError, STAKING_ESCROW};
use call_protocol::account::AccountState;
use call_protocol::instructions::{Instruction, InstructionResult};
use crate::validator::ValidatorStateManager;

// ── Validator instruction helpers ─────────────────────────────────────

pub(crate) fn is_validator_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::ValidatorStake { .. }
            | Instruction::ValidatorUnstake { .. }
            | Instruction::ValidatorClaimUnbonded { .. }
    )
}

pub(crate) fn execute_validator_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    validator_state: &mut ValidatorStateManager,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::ValidatorStake {
            ed25519_pubkey,
            self_stake,
        } => {
            // Verify sender has sufficient balance
            let balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
            if balance < *self_stake {
                return Err(ConsensusError::InvalidBlock(
                    "validator stake: insufficient balance".into(),
                ));
            }
            // Transfer stake to escrow (Cosmos-style module account)
            account
                .transfer(call_protocol::CALL_ASSET_ID, sender, STAKING_ESCROW, *self_stake)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator stake: {e}")))?;
            // Register validator
            validator_state.set_current_block(current_block_height);
            validator_state
                .stake(sender, *ed25519_pubkey, *self_stake)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator stake: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::ValidatorUnstake { validator_id } => {
            validator_state.set_current_block(current_block_height);
            validator_state
                .unstake(*validator_id, sender)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator unstake: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::ValidatorClaimUnbonded { validator_id } => {
            validator_state.set_current_block(current_block_height);
            let (amount, recipient) = validator_state
                .claim_unbonded(*validator_id)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator claim: {e}")))?;
            // Return staked tokens from escrow to the original staker
            account
                .transfer(call_protocol::CALL_ASSET_ID, STAKING_ESCROW, recipient, amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator claim: {e}")))?;
            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock(
            "not a validator instruction".into(),
        )),
    }
}

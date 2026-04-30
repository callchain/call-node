//! EVM-based protocol instruction execution.
//!
//! Asset instructions (Transfer, BatchTransfer, Approve, TransferFrom,
//! Mint, Burn) are executed by reading/writing EVM storage directly,
//! using the same slot layout as the AssetPrecompile (0x201).
//!
//! This replaces the legacy `AccountState` / `AssetRegistry` mutation path
//! so that the block state_root captures all state changes.

use call_evm::EvmState;
use call_precompiles::{
    address_to_u256, read_string32, slot_allowance, slot_asset_meta, slot_balance,
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256,
    write_string32,
    AGENT_ADDRESS, ASSET_ADDRESS, BRIDGE_ADDRESS, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
    ORACLE_ADDRESS, SHIELDED_ADDRESS, VALIDATOR_ADDRESS,
};
use call_shielded::ShieldedState;
use call_precompiles::storage::storage_slot;
use call_primitives::{Address, U256};
use call_protocol::instructions::{Instruction, InstructionResult};

use crate::{ConsensusError, STAKING_ESCROW};

// ── Constants ─────────────────────────────────────────────────────────

/// Default unbonding period in blocks (~8.4h at 250ms block time)
const UNBONDING_PERIOD_BLOCKS: u64 = 120_960;
/// Minimum self-stake to become a validator
const MIN_SELF_STAKE: u128 = 1_000_000;
/// Proposal deposit in CALL
const PROPOSAL_DEPOSIT: u128 = 10_000;
/// Governance timelock in blocks
const GOV_TIMELOCK_BLOCKS: u64 = 100;
/// Governance quorum threshold (basis points)
const GOV_QUORUM_BPS: u128 = 3_333;

/// Whether this instruction is handled by the EVM executor.
pub fn is_evm_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::Transfer { .. }
            | Instruction::BatchTransfer { .. }
            | Instruction::Approve { .. }
            | Instruction::TransferFrom { .. }
            | Instruction::Mint { .. }
            | Instruction::Burn { .. }
            | Instruction::UpdateCompliance { .. }
            | Instruction::ValidatorStake { .. }
            | Instruction::ValidatorUnstake { .. }
            | Instruction::ValidatorClaimUnbonded { .. }
            | Instruction::OracleSubmit { .. }
            | Instruction::GovernanceSubmitProposal { .. }
            | Instruction::GovernanceVote { .. }
            | Instruction::GovernanceQueue { .. }
            | Instruction::GovernanceExecute { .. }
            | Instruction::GovernanceEmergencyPause { .. }
            | Instruction::GovernanceEmergencyResume
            | Instruction::BridgeDeposit { .. }
            | Instruction::ExternalBridgeDeposit { .. }
            | Instruction::ExternalBridgeWithdraw { .. }
            | Instruction::ChallengeBridgeDeposit { .. }
            | Instruction::BridgeToEvm { .. }
            | Instruction::BridgeToProtocol { .. }
            | Instruction::AgentPay { .. }
            | Instruction::AgentBatchPay { .. }
            | Instruction::AgentCall { .. }
            | Instruction::AgentBridgeDeposit { .. }
            | Instruction::RegisterAgent { .. }
            | Instruction::GrantAgentBalance { .. }
            | Instruction::RevokeAgentBalance { .. }
            | Instruction::RegisterAsset { .. }
            | Instruction::EvmIssuerMint { .. }
            | Instruction::ShieldedDeposit { .. }
            | Instruction::ShieldedWithdraw { .. }
            | Instruction::ShieldedTransfer { .. }
    )
}

/// Execute a single instruction against EVM storage.
pub fn execute_instruction_on_evm(
    instruction: &Instruction,
    sender: Address,
    evm_state: &mut EvmState,
    shielded_state: &mut ShieldedState,
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    validators: Option<&[Address]>,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::Transfer {
            asset_id,
            to,
            amount,
            memo,
        } => {
            if let Some(m) = memo {
                m.validate()
                    .map_err(|e| ConsensusError::InvalidBlock(format!("memo: {e}")))?;
            }
            exec_transfer(evm_state, *asset_id, sender, *to, *amount)
        }
        Instruction::BatchTransfer {
            asset_id,
            payments,
        } => exec_batch_transfer(evm_state, *asset_id, sender, payments),
        Instruction::Approve {
            asset_id,
            spender,
            amount,
        } => exec_approve(evm_state, *asset_id, sender, *spender, *amount),
        Instruction::TransferFrom {
            asset_id,
            from,
            to,
            amount,
        } => exec_transfer_from(evm_state, *asset_id, *from, *to, sender, *amount),
        Instruction::Mint {
            asset_id,
            to,
            amount,
        } => exec_mint(evm_state, *asset_id, sender, *to, *amount),
        Instruction::Burn {
            asset_id,
            from,
            amount,
        } => exec_burn(evm_state, *asset_id, sender, *from, *amount),
        Instruction::UpdateCompliance {
            asset_id,
            target,
            status,
        } => exec_update_compliance(evm_state, *asset_id, sender, *target, *status),
        Instruction::ValidatorStake {
            ed25519_pubkey,
            self_stake,
        } => exec_validator_stake(evm_state, sender, *ed25519_pubkey, *self_stake),
        Instruction::ValidatorUnstake { validator_id } => {
            exec_validator_unstake(evm_state, sender, *validator_id, current_block_height)
        }
        Instruction::ValidatorClaimUnbonded { validator_id } => {
            exec_validator_claim_unbonded(evm_state, sender, *validator_id, current_block_height)
        }
        Instruction::OracleSubmit {
            asset_id,
            price,
            block_number,
            timestamp,
            signature,
            sources,
        } => exec_oracle_submit(
            evm_state,
            sender,
            *asset_id,
            *price,
            *block_number,
            *timestamp,
            signature,
            sources,
        ),
        Instruction::GovernanceSubmitProposal {
            proposal_type,
            title,
            description,
            execution_data,
        } => exec_governance_submit_proposal(
            evm_state,
            sender,
            proposal_type.clone(),
            title.clone(),
            description.clone(),
            execution_data.clone(),
        ),
        Instruction::GovernanceVote { proposal_id, vote } => {
            exec_governance_vote(evm_state, sender, *proposal_id, vote.clone())
        }
        Instruction::GovernanceQueue { proposal_id } => {
            exec_governance_queue(evm_state, sender, *proposal_id, current_block_height)
        }
        Instruction::GovernanceExecute { proposal_id } => {
            exec_governance_execute(evm_state, sender, *proposal_id, current_block_height)
        }
        Instruction::GovernanceEmergencyPause { reason } => {
            exec_governance_emergency_pause(evm_state, sender, reason.clone())
        }
        Instruction::GovernanceEmergencyResume => {
            exec_governance_emergency_resume(evm_state, sender)
        }
        Instruction::BridgeDeposit {
            source_chain,
            target_address,
            amount,
            asset_id,
            proof,
        } => exec_bridge_deposit(
            evm_state,
            sender,
            *source_chain,
            *target_address,
            *amount,
            *asset_id,
            proof,
            current_block_height,
            bridge_config,
            validators,
        ),
        Instruction::ExternalBridgeDeposit {
            source_tx_hash,
            source_chain,
            source_block_number,
            external_sender,
            recipient,
            asset_id,
            amount,
            validator_signatures,
        } => exec_external_bridge_deposit(
            evm_state,
            sender,
            *source_tx_hash,
            *source_chain,
            *source_block_number,
            external_sender.clone(),
            *recipient,
            *asset_id,
            *amount,
            validator_signatures,
            current_block_height,
            bridge_config,
            validators,
        ),
        Instruction::ExternalBridgeWithdraw {
            target_chain,
            target_address,
            asset_id,
            sender: withdraw_sender,
            amount,
        } => exec_external_bridge_withdraw(
            evm_state,
            *target_chain,
            target_address.clone(),
            *asset_id,
            *withdraw_sender,
            *amount,
            current_block_height,
            bridge_config,
        ),
        Instruction::ChallengeBridgeDeposit {
            source_tx_hash,
            proof: _,
        } => exec_challenge_bridge_deposit(evm_state, *source_tx_hash, current_block_height),
        Instruction::BridgeToEvm {
            asset_id,
            to,
            amount,
        } => exec_bridge_to_evm(
            evm_state,
            sender,
            *asset_id,
            *to,
            *amount,
            current_block_height,
            bridge_config,
            evm_executor,
        ),
        Instruction::BridgeToProtocol {
            asset_id,
            to,
            amount,
        } => exec_bridge_to_protocol(
            evm_state,
            sender,
            *asset_id,
            *to,
            *amount,
            current_block_height,
            bridge_config,
            evm_executor,
        ),
        Instruction::AgentPay { payment } => exec_agent_pay(
            evm_state,
            sender,
            payment,
            current_block_height,
        ),
        Instruction::AgentBatchPay { payments } => {
            exec_agent_batch_pay(evm_state, sender, payments, current_block_height)
        }
        Instruction::AgentCall {
            agent_id,
            target,
            data,
        } => exec_agent_call(
            evm_state,
            sender,
            *agent_id,
            *target,
            data,
            current_block_height,
            evm_executor,
        ),
        Instruction::AgentBridgeDeposit {
            agent_id,
            asset_id,
            amount,
            target_chain,
            target_address,
        } => exec_agent_bridge_deposit(
            evm_state,
            sender,
            *agent_id,
            *asset_id,
            *amount,
            *target_chain,
            target_address,
            current_block_height,
            bridge_config,
            evm_executor,
        ),
        Instruction::RegisterAgent {
            pubkey,
            name,
            url,
        } => exec_register_agent(
            evm_state,
            sender,
            pubkey,
            name,
            url,
            current_block_height,
        ),
        Instruction::GrantAgentBalance {
            agent_id,
            asset_id,
            amount,
        } => exec_grant_agent_balance(evm_state, sender, *agent_id, *asset_id, *amount),
        Instruction::RevokeAgentBalance {
            agent_id,
            asset_id,
        } => exec_revoke_agent_balance(evm_state, sender, *agent_id, *asset_id),
        Instruction::RegisterAsset {
            symbol,
            name,
            decimals,
            max_supply,
        } => exec_register_asset(
            evm_state,
            sender,
            symbol,
            name,
            *decimals,
            *max_supply,
            evm_executor,
        ),
        Instruction::EvmIssuerMint {
            asset_id,
            to,
            amount,
        } => exec_evm_issuer_mint(evm_state, sender, *asset_id, *to, *amount, evm_executor),
        Instruction::ShieldedDeposit {
            asset_id,
            amount,
            commitment,
            encrypted_note,
        } => exec_shielded_deposit(
            evm_state,
            shielded_state,
            sender,
            *asset_id,
            *amount,
            *commitment,
            encrypted_note,
        ),
        Instruction::ShieldedWithdraw {
            asset_id,
            target,
            amount,
            proof,
            nullifier,
        } => exec_shielded_withdraw(
            evm_state,
            shielded_state,
            *asset_id,
            *target,
            *amount,
            proof,
            *nullifier,
        ),
        Instruction::ShieldedTransfer {
            asset_id,
            proof,
            nullifiers,
            commitments,
            encrypted_notes,
        } => exec_shielded_transfer(
            evm_state,
            shielded_state,
            *asset_id,
            proof,
            nullifiers,
            commitments,
            encrypted_notes,
        ),
        _ => Err(ConsensusError::InvalidBlock(
            "instruction not handled by EVM executor".into(),
        )),
    }
}

// ── Asset status check ────────────────────────────────────────────────

fn check_asset_active(evm_state: &EvmState, asset_id: u64) -> Result<(), ConsensusError> {
    let status = evm_state
        .get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
        .to_be_bytes::<32>()[31];
    match status {
        0 => Ok(()), // Active
        1 => Err(ConsensusError::InvalidBlock("asset is frozen".into())),
        2 => Err(ConsensusError::InvalidBlock("asset is delisted".into())),
        _ => Err(ConsensusError::InvalidBlock("asset is not active".into())),
    }
}

fn get_compliance_policy(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state
        .get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"))
        .to_be_bytes::<32>()[31]
}

fn check_address_compliance(
    evm_state: &EvmState,
    address: &Address,
    policy_id: u8,
) -> Result<(), ConsensusError> {
    if policy_id == 0 {
        return Ok(());
    }
    let status = read_compliance_status(evm_state, *address, policy_id);
    if status == 0 {
        Ok(())
    } else {
        Err(ConsensusError::InvalidBlock("compliance check failed".into()))
    }
}

// ── Storage slot helpers (shared with precompiles) ────────────────────

fn slot_validator_count() -> U256 {
    U256::ZERO
}

fn slot_validator_by_addr(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"validator_id"])
}

fn slot_validator_addr(index: u64) -> U256 {
    storage_slot(&[b"validators"]) + U256::from(index)
}

fn slot_validator_stake(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"stake"])
}

fn slot_validator_pubkey(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"pubkey"])
}

fn slot_validator_status(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"status"])
}

fn slot_validator_unbond_height(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"unbond_at"])
}

fn slot_unbonding_count() -> U256 {
    U256::from(1)
}

fn slot_unbonding(index: u64) -> U256 {
    storage_slot(&[b"unbonding"]) + U256::from(index)
}

fn slot_oracle(asset_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], suffix])
}

fn slot_compliance(addr: Address, policy_id: u8) -> U256 {
    storage_slot(&[addr.as_slice(), &[policy_id]])
}

fn slot_gov_proposal_count() -> U256 {
    U256::ZERO
}

fn slot_gov_proposal(proposal_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], suffix])
}

fn slot_gov_voter(proposal_id: u64, voter: Address) -> U256 {
    storage_slot(&[b"vote", &proposal_id.to_be_bytes()[..], voter.as_slice()])
}

fn slot_gov_paused() -> U256 {
    storage_slot(&[b"paused"])
}

fn slot_gov_pause_reason() -> U256 {
    storage_slot(&[b"pause_reason"])
}

fn slot_gov_proposal_deposit() -> U256 {
    storage_slot(&[b"deposit_fee"])
}

// ── Bridge storage slot helpers ───────────────────────────────────────

fn slot_bridge_total_deposits(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"deposits"])
}

fn slot_bridge_total_withdrawals(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"withdrawals"])
}

fn slot_bridge_paused(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"paused"])
}

fn slot_bridge_daily_used(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"daily"])
}

fn slot_bridge_daily_day(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"daily_day"])
}

fn slot_bridge_external_paused() -> U256 {
    storage_slot(&[b"external_paused"])
}

fn slot_bridge_processed(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"processed", &source_tx_hash])
}

pub(crate) fn slot_bridge_contract(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"contract"])
}

fn slot_bridge_pending_count() -> U256 {
    storage_slot(&[b"pending_count"])
}

fn slot_bridge_pending_hash(index: u64) -> U256 {
    storage_slot(&[b"pending_list"]) + U256::from(index)
}

fn slot_bridge_pending_status(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_status", &source_tx_hash])
}

fn slot_bridge_pending_recipient(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_recipient", &source_tx_hash])
}

fn slot_bridge_pending_asset(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_asset", &source_tx_hash])
}

fn slot_bridge_pending_amount(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_amount", &source_tx_hash])
}

fn slot_bridge_pending_block(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_block", &source_tx_hash])
}

fn slot_bridge_withdrawal_period_start() -> U256 {
    storage_slot(&[b"withdrawal_start"])
}

fn slot_bridge_withdrawal_period_used(asset_id: u64) -> U256 {
    storage_slot(&[b"withdrawal_period", &asset_id.to_be_bytes()[..]])
}

// ── Bridge helpers ────────────────────────────────────────────────────

fn bridge_is_paused(evm_state: &EvmState, asset_id: u64) -> bool {
    evm_state
        .get_storage(&BRIDGE_ADDRESS, slot_bridge_paused(asset_id))
        .to_be_bytes::<32>()[31]
        == 1
}

fn bridge_is_external_paused(evm_state: &EvmState) -> bool {
    evm_state
        .get_storage(&BRIDGE_ADDRESS, slot_bridge_external_paused())
        .to_be_bytes::<32>()[31]
        == 1
}

fn bridge_check_per_tx_limit(amount: u128, max_per_tx: u128) -> Result<(), ConsensusError> {
    if amount > max_per_tx {
        return Err(ConsensusError::InvalidBlock(format!(
            "bridge: amount {amount} exceeds per-tx limit {max_per_tx}"
        )));
    }
    Ok(())
}

fn bridge_check_and_update_daily_limit(
    evm_state: &mut EvmState,
    asset_id: u64,
    amount: u128,
    daily_limit: u128,
    current_block: u64,
    blocks_per_day: u64,
) -> Result<(), ConsensusError> {
    let day_slot = slot_bridge_daily_day(asset_id);
    let used_slot = slot_bridge_daily_used(asset_id);
    let stored_day = u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, day_slot));
    let mut used = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, used_slot));

    if current_block >= stored_day + blocks_per_day {
        used = 0;
        evm_state.set_storage(BRIDGE_ADDRESS, day_slot, u64_to_u256(current_block));
    }

    let new_used = used
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("bridge: daily limit overflow".into()))?;
    if new_used > daily_limit {
        return Err(ConsensusError::InvalidBlock(format!(
            "bridge: daily limit exceeded for asset {asset_id}: used {used}, adding {amount}, limit {daily_limit}"
        )));
    }

    evm_state.set_storage(BRIDGE_ADDRESS, used_slot, u128_to_u256(new_used));
    Ok(())
}

fn bridge_check_and_update_withdrawal_period(
    evm_state: &mut EvmState,
    asset_id: u64,
    amount: u128,
    max_per_period: u128,
    current_block: u64,
    challenge_period_blocks: u64,
) -> Result<(), ConsensusError> {
    let start_slot = slot_bridge_withdrawal_period_start();
    let used_slot = slot_bridge_withdrawal_period_used(asset_id);
    let stored_start = u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, start_slot));
    let mut used = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, used_slot));

    if current_block >= stored_start + challenge_period_blocks {
        used = 0;
        evm_state.set_storage(BRIDGE_ADDRESS, start_slot, u64_to_u256(current_block));
    }

    let new_used = used
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("bridge: withdrawal period overflow".into()))?;
    if new_used > max_per_period {
        return Err(ConsensusError::InvalidBlock(format!(
            "bridge: withdrawal period limit exceeded for asset {asset_id}: used {used}, adding {amount}, limit {max_per_period}"
        )));
    }

    evm_state.set_storage(BRIDGE_ADDRESS, used_slot, u128_to_u256(new_used));
    Ok(())
}

fn bridge_asset_registered(evm_state: &EvmState, asset_id: u64) -> bool {
    let symbol = evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol"));
    symbol != U256::ZERO
}

fn bridge_get_evm_contract(evm_state: &EvmState, asset_id: u64) -> Option<Address> {
    let addr_u256 = evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_contract(asset_id));
    if addr_u256 == U256::ZERO {
        None
    } else {
        Some(u256_to_address(addr_u256))
    }
}

fn bridge_mark_processed(evm_state: &mut EvmState, source_tx_hash: [u8; 32], block: u64) {
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        slot_bridge_processed(source_tx_hash),
        u64_to_u256(block),
    );
}

fn bridge_is_processed(evm_state: &EvmState, source_tx_hash: [u8; 32]) -> bool {
    u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash))) != 0
}

// ── BridgeToEvm ───────────────────────────────────────────────────────

fn exec_bridge_to_evm(
    evm_state: &mut EvmState,
    sender: Address,
    asset_id: u64,
    to: Address,
    amount: u128,
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    if asset_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "BridgeToEvm: asset 0 (USD) is not bridgeable".into(),
        ));
    }

    let config = bridge_config.ok_or_else(|| {
        ConsensusError::InvalidBlock("BridgeToEvm: bridge config required".into())
    })?;

    if !bridge_asset_registered(evm_state, asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToEvm: asset {asset_id} not registered"
        )));
    }

    let status = evm_state
        .get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
        .to_be_bytes::<32>()[31];
    if status != 0 {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToEvm: asset {asset_id} is not active (status: {status})"
        )));
    }

    if bridge_is_paused(evm_state, asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToEvm: bridge paused for asset {asset_id}"
        )));
    }

    bridge_check_per_tx_limit(amount, config.max_per_tx)?;
    bridge_check_and_update_daily_limit(
        evm_state,
        asset_id,
        amount,
        config.daily_limit_per_asset,
        current_block_height,
        config.blocks_per_day,
    )?;

    // Deduct protocol balance from sender
    let sender_slot = slot_balance(asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    let sender_bal = sender_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock(format!(
            "BridgeToEvm: insufficient protocol balance for asset {asset_id}: have {sender_bal}, need {amount}"
        )))?;
    evm_state.set_storage(ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

    // Bridge to EVM
    let amount_u256 = call_evm::U256::from(amount);
    let call_asset_id = call_protocol::CALL_ASSET_ID;
    if asset_id == call_asset_id {
        let current = evm_state.get_balance(&to);
        evm_state.set_balance(to, current + amount_u256);
    } else {
        let contract = bridge_get_evm_contract(evm_state, asset_id).ok_or_else(|| {
            ConsensusError::InvalidBlock(format!(
                "BridgeToEvm: no EVM contract registered for asset {asset_id}"
            ))
        })?;
        let executor = evm_executor.ok_or_else(|| {
            ConsensusError::InvalidBlock("BridgeToEvm: EVM executor required for ERC-20 mint".into())
        })?;
        let result = executor
            .evm_call_bridge_mint(
                call_protocol::BRIDGE_EVM_ADDRESS,
                contract,
                evm_state,
                to,
                amount_u256,
            )
            .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e:?}")))?;
        if !result.success {
            return Err(ConsensusError::InvalidBlock(
                "BridgeToEvm: EVM operation reverted".into(),
            ));
        }
    }

    // Record deposit total
    let deposits_slot = slot_bridge_total_deposits(asset_id);
    let deposits = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, deposits_slot));
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        deposits_slot,
        u128_to_u256(deposits.saturating_add(amount)),
    );

    Ok(InstructionResult::Success)
}

// ── BridgeToProtocol ──────────────────────────────────────────────────

fn exec_bridge_to_protocol(
    evm_state: &mut EvmState,
    sender: Address,
    asset_id: u64,
    to: Address,
    amount: u128,
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    if asset_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "BridgeToProtocol: asset 0 (USD) is not bridgeable".into(),
        ));
    }

    let config = bridge_config.ok_or_else(|| {
        ConsensusError::InvalidBlock("BridgeToProtocol: bridge config required".into())
    })?;

    if !bridge_asset_registered(evm_state, asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToProtocol: asset {asset_id} not registered"
        )));
    }

    let status = evm_state
        .get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
        .to_be_bytes::<32>()[31];
    if status != 0 {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToProtocol: asset {asset_id} is not active (status: {status})"
        )));
    }

    if bridge_is_paused(evm_state, asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "BridgeToProtocol: bridge paused for asset {asset_id}"
        )));
    }

    bridge_check_per_tx_limit(amount, config.max_per_tx)?;
    bridge_check_and_update_daily_limit(
        evm_state,
        asset_id,
        amount,
        config.daily_limit_per_asset,
        current_block_height,
        config.blocks_per_day,
    )?;

    let amount_u256 = call_evm::U256::from(amount);
    let call_asset_id = call_protocol::CALL_ASSET_ID;

    // Withdraw from EVM
    if asset_id == call_asset_id {
        let evm_balance = evm_state.get_balance(&sender);
        if evm_balance < amount_u256 {
            return Err(ConsensusError::InvalidBlock(format!(
                "BridgeToProtocol: insufficient EVM native balance for CALL: have {evm_balance}, need {amount_u256}"
            )));
        }
        evm_state.set_balance(sender, evm_balance - amount_u256);
    } else {
        let contract = bridge_get_evm_contract(evm_state, asset_id).ok_or_else(|| {
            ConsensusError::InvalidBlock(format!(
                "BridgeToProtocol: no EVM contract registered for asset {asset_id}"
            ))
        })?;
        let executor = evm_executor.ok_or_else(|| {
            ConsensusError::InvalidBlock("BridgeToProtocol: EVM executor required for ERC-20 burn".into())
        })?;
        let result = executor
            .evm_call_bridge_burn(
                sender,
                contract,
                evm_state,
                amount_u256,
            )
            .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e:?}")))?;
        if !result.success {
            return Err(ConsensusError::InvalidBlock(
                "BridgeToProtocol: EVM operation reverted".into(),
            ));
        }
    }

    // Credit protocol balance
    let to_slot = slot_balance(asset_id, to);
    let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
    let to_bal = to_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("BridgeToProtocol: balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

    // Record withdrawal total
    let withdrawals_slot = slot_bridge_total_withdrawals(asset_id);
    let withdrawals = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, withdrawals_slot));
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        withdrawals_slot,
        u128_to_u256(withdrawals.saturating_add(amount)),
    );

    Ok(InstructionResult::Success)
}

// ── BridgeOp wrappers (for Block::execute Step 3) ─────────────────────

pub(crate) fn exec_bridge_op_deposit(
    evm_state: &mut EvmState,
    op: &call_bridge::BridgeOp,
    current_block_height: u64,
    bridge_config: &call_bridge::BridgeConfig,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<(), ConsensusError> {
    let call_bridge::BridgeOp::DepositToEvm {
        asset_id,
        from,
        to,
        amount,
    } = op
    else {
        return Err(ConsensusError::InvalidBlock(
            "exec_bridge_op_deposit: not a deposit op".into(),
        ));
    };
    exec_bridge_to_evm(
        evm_state,
        *from,
        *asset_id,
        *to,
        *amount,
        current_block_height,
        Some(bridge_config),
        evm_executor,
    )?;
    Ok(())
}

pub(crate) fn exec_bridge_op_withdraw(
    evm_state: &mut EvmState,
    op: &call_bridge::BridgeOp,
    current_block_height: u64,
    bridge_config: &call_bridge::BridgeConfig,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<(), ConsensusError> {
    let call_bridge::BridgeOp::WithdrawToProtocol {
        asset_id,
        from,
        to,
        amount,
    } = op
    else {
        return Err(ConsensusError::InvalidBlock(
            "exec_bridge_op_withdraw: not a withdraw op".into(),
        ));
    };
    exec_bridge_to_protocol(
        evm_state,
        *from,
        *asset_id,
        *to,
        *amount,
        current_block_height,
        Some(bridge_config),
        evm_executor,
    )?;
    Ok(())
}

// ── ExternalBridgeDeposit ─────────────────────────────────────────────

fn exec_external_bridge_deposit(
    evm_state: &mut EvmState,
    _sender: Address,
    source_tx_hash: [u8; 32],
    source_chain: u8,
    source_block_number: u64,
    external_sender: Vec<u8>,
    recipient: Address,
    asset_id: u64,
    amount: u128,
    validator_signatures: &[(u32, Vec<u8>)],
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    validators: Option<&[Address]>,
) -> Result<InstructionResult, ConsensusError> {
    let config = bridge_config.ok_or_else(|| {
        ConsensusError::InvalidBlock("ExternalBridgeDeposit: bridge config required".into())
    })?;
    let validators = validators.ok_or_else(|| {
        ConsensusError::InvalidBlock("ExternalBridgeDeposit: validators required".into())
    })?;

    if bridge_is_external_paused(evm_state) {
        return Err(ConsensusError::InvalidBlock(
            "ExternalBridgeDeposit: external bridge paused".into(),
        ));
    }

    let chain = match source_chain {
        0 => call_bridge::ExternalChain::EthereumMainnet,
        1 => call_bridge::ExternalChain::Arbitrum,
        _ => return Err(ConsensusError::InvalidBlock("bridge: unknown source chain".into())),
    };

    if !config.allowed_assets.contains(&asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "ExternalBridgeDeposit: asset {asset_id} not allowed"
        )));
    }

    if bridge_is_processed(evm_state, source_tx_hash) {
        return Err(ConsensusError::InvalidBlock(
            "ExternalBridgeDeposit: source tx already processed".into(),
        ));
    }

    // Verify signatures
    let signatures: Vec<call_bridge::BridgeSignature> = validator_signatures
        .iter()
        .map(|(idx, sig)| call_bridge::BridgeSignature {
            validator_index: *idx,
            signature: sig.as_slice().try_into().unwrap_or([0u8; 65]),
        })
        .collect();
    let op = call_bridge::ExternalBridgeOp::Deposit {
        source_chain: chain,
        source_tx_hash: call_primitives::B256::from(source_tx_hash),
        source_block_number,
        sender: external_sender,
        recipient,
        asset_id,
        amount,
        signatures,
    };
    call_bridge::verify_bridge_signatures(&op, validators, config.min_validator_signatures)
        .map_err(|e| ConsensusError::InvalidBlock(format!("ExternalBridgeDeposit: {e}")))?;

    bridge_check_per_tx_limit(amount, config.max_per_tx)?;
    bridge_check_and_update_daily_limit(
        evm_state,
        asset_id,
        amount,
        config.daily_limit_per_asset,
        current_block_height,
        config.blocks_per_day,
    )?;

    // Apply bridge fee
    let fee = config.bridge_fee;
    let net_amount = if fee >= amount {
        return Err(ConsensusError::InvalidBlock(format!(
            "ExternalBridgeDeposit: bridge fee {fee} exceeds amount {amount}"
        )));
    } else {
        amount - fee
    };

    // Record fee
    if fee > 0 {
        let fee_slot = slot_bridge_total_withdrawals(asset_id); // reuse withdrawals slot for fee tracking? No, better to use a separate slot. For now just ignore fee tracking.
        let _ = fee_slot;
    }

    // Mark as processed
    bridge_mark_processed(evm_state, source_tx_hash, current_block_height);

    // Credit recipient immediately (challenge period deferred to later refinement)
    let recipient_slot = slot_balance(asset_id, recipient);
    let recipient_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, recipient_slot));
    let recipient_bal = recipient_bal
        .checked_add(net_amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("ExternalBridgeDeposit: balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, recipient_slot, u128_to_u256(recipient_bal));

    // Record deposit total
    let deposits_slot = slot_bridge_total_deposits(asset_id);
    let deposits = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, deposits_slot));
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        deposits_slot,
        u128_to_u256(deposits.saturating_add(net_amount)),
    );

    Ok(InstructionResult::Success)
}

// ── BridgeDeposit (legacy with proof) ─────────────────────────────────

fn exec_bridge_deposit(
    evm_state: &mut EvmState,
    _sender: Address,
    source_chain: u64,
    target_address: Address,
    amount: u128,
    asset_id: u64,
    proof: &[u8],
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    validators: Option<&[Address]>,
) -> Result<InstructionResult, ConsensusError> {
    if proof.is_empty() {
        return Err(ConsensusError::InvalidBlock(
            "BridgeDeposit: empty proof".into(),
        ));
    }
    let deposit_proof: call_bridge::BridgeDepositProof = serde_json::from_slice(proof)
        .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeDeposit: invalid proof format: {e}")))?;

    exec_external_bridge_deposit(
        evm_state,
        _sender,
        deposit_proof.source_tx_hash,
        source_chain as u8,
        deposit_proof.source_block_number,
        deposit_proof.external_sender,
        target_address,
        asset_id,
        amount,
        &deposit_proof.signatures,
        current_block_height,
        bridge_config,
        validators,
    )
}

// ── ExternalBridgeWithdraw ────────────────────────────────────────────

fn exec_external_bridge_withdraw(
    evm_state: &mut EvmState,
    target_chain: u8,
    _target_address: Vec<u8>,
    asset_id: u64,
    sender: Address,
    amount: u128,
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
) -> Result<InstructionResult, ConsensusError> {
    let config = bridge_config.ok_or_else(|| {
        ConsensusError::InvalidBlock("ExternalBridgeWithdraw: bridge config required".into())
    })?;

    if bridge_is_external_paused(evm_state) {
        return Err(ConsensusError::InvalidBlock(
            "ExternalBridgeWithdraw: external bridge paused".into(),
        ));
    }

    let _chain = match target_chain {
        0 => call_bridge::ExternalChain::EthereumMainnet,
        1 => call_bridge::ExternalChain::Arbitrum,
        _ => return Err(ConsensusError::InvalidBlock("bridge: unknown target chain".into())),
    };

    if !config.allowed_assets.contains(&asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "ExternalBridgeWithdraw: asset {asset_id} not allowed"
        )));
    }

    bridge_check_per_tx_limit(amount, config.max_per_tx)?;
    bridge_check_and_update_daily_limit(
        evm_state,
        asset_id,
        amount,
        config.daily_limit_per_asset,
        current_block_height,
        config.blocks_per_day,
    )?;
    bridge_check_and_update_withdrawal_period(
        evm_state,
        asset_id,
        amount,
        config.max_external_withdraw_per_period,
        current_block_height,
        config.challenge_period_blocks,
    )?;

    // Apply bridge fee
    let fee = config.bridge_fee;
    let total_deduction = amount.checked_add(fee)
        .ok_or_else(|| ConsensusError::InvalidBlock("ExternalBridgeWithdraw: fee exceeds amount".into()))?;

    // Check and deduct protocol balance
    let sender_slot = slot_balance(asset_id, sender);
    let balance = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    if balance < total_deduction {
        return Err(ConsensusError::InvalidBlock(format!(
            "ExternalBridgeWithdraw: insufficient balance for asset {asset_id}: have {balance}, need {total_deduction}"
        )));
    }
    evm_state.set_storage(
        ASSET_ADDRESS,
        sender_slot,
        u128_to_u256(balance - total_deduction),
    );

    // Record withdrawal total
    let withdrawals_slot = slot_bridge_total_withdrawals(asset_id);
    let withdrawals = u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, withdrawals_slot));
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        withdrawals_slot,
        u128_to_u256(withdrawals.saturating_add(amount)),
    );

    Ok(InstructionResult::Success)
}

// ── ChallengeBridgeDeposit ────────────────────────────────────────────

fn exec_challenge_bridge_deposit(
    evm_state: &mut EvmState,
    source_tx_hash: [u8; 32],
    _current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    if !bridge_is_processed(evm_state, source_tx_hash) {
        return Err(ConsensusError::InvalidBlock(
            "ChallengeBridgeDeposit: source tx not processed or no pending deposit found".into(),
        ));
    }

    // In the EVM-only simplified model, deposits are credited immediately.
    // A full challenge-period queue in EVM storage is a future refinement.
    // For now, challenging a processed deposit is not supported.
    Err(ConsensusError::InvalidBlock(
        "ChallengeBridgeDeposit: challenge period queue not yet implemented in EVM-only mode".into(),
    ))
}

// ── Agent storage slot helpers ────────────────────────────────────────

pub(crate) fn slot_agent_count() -> U256 {
    U256::ZERO
}

pub(crate) fn slot_agent_owner(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"owner"])
}

pub(crate) fn slot_agent_pubkey(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"pubkey"])
}

pub(crate) fn slot_agent_name(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"name"])
}

pub(crate) fn slot_agent_url(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"url"])
}

pub(crate) fn slot_agent_perms(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"perms"])
}

pub(crate) fn slot_agent_registered_at(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"block"])
}

pub(crate) fn slot_agent_balance(agent_id: u64, asset_id: u64) -> U256 {
    storage_slot(&[b"abalance", &agent_id.to_be_bytes()[..], &asset_id.to_be_bytes()[..]])
}

// Pack agent permissions into a single U256:
// bytes 0..16  = per_tx_limit (u128)
// bytes 16..24 = expires_at (u64)
// byte 31      = flags (bit 0 = allow asset 1, bit 1 = allow all protocols)
pub(crate) fn pack_agent_perms(per_tx_limit: u128, expires_at: u64, flags: u8) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..24].copy_from_slice(&expires_at.to_be_bytes());
    packed[31] = flags;
    U256::from_be_slice(&packed)
}

pub(crate) fn unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit = u128::from_be_bytes(bytes[0..16].try_into().unwrap());
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

pub fn agent_exists(evm_state: &EvmState, agent_id: u64) -> bool {
    evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)) != U256::ZERO
}

pub fn agent_get_owner(evm_state: &EvmState, agent_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)))
}

fn agent_check_owner(evm_state: &EvmState, agent_id: u64, sender: Address) -> Result<(), ConsensusError> {
    let owner = agent_get_owner(evm_state, agent_id);
    if owner != sender {
        return Err(ConsensusError::InvalidBlock(
            "agent: sender is not owner".into(),
        ));
    }
    Ok(())
}

pub fn agent_get_balance(evm_state: &EvmState, agent_id: u64, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id)))
}

pub fn agent_set_balance(evm_state: &mut EvmState, agent_id: u64, asset_id: u64, amount: u128) {
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_balance(agent_id, asset_id),
        u128_to_u256(amount),
    );
}

fn agent_check_permissions(
    evm_state: &EvmState,
    agent_id: u64,
    asset_id: u64,
    amount: u128,
    current_block: u64,
) -> Result<(), ConsensusError> {
    let perms = evm_state.get_storage(&AGENT_ADDRESS, slot_agent_perms(agent_id));
    let (per_tx_limit, expires_at, flags) = unpack_agent_perms(perms);

    if expires_at != 0 && current_block > expires_at {
        return Err(ConsensusError::InvalidBlock(
            "agent: permissions expired".into(),
        ));
    }
    if amount > per_tx_limit {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent: amount {amount} exceeds per-tx limit {per_tx_limit}"
        )));
    }
    if asset_id != call_protocol::CALL_ASSET_ID && (flags & 1) == 0 {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent: asset {asset_id} not allowed"
        )));
    }
    Ok(())
}

// ── RegisterAgent ─────────────────────────────────────────────────────

fn exec_register_agent(
    evm_state: &mut EvmState,
    sender: Address,
    pubkey: &[u8],
    name: &str,
    url: &str,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    if pubkey.len() != 64 {
        return Err(ConsensusError::InvalidBlock(
            "RegisterAgent: pubkey must be 64 bytes".into(),
        ));
    }

    // Deduct registration fee (use base fee from fee params; simplified: 0 for now)
    // In EVM-only mode we read fee from a governance slot; for now assume 0.

    let count = u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_count()));
    let agent_id = count;
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(count + 1));

    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_owner(agent_id),
        address_to_u256(sender),
    );
    let pk_hash = call_crypto::keccak256(pubkey);
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_pubkey(agent_id),
        U256::from_be_slice(pk_hash.as_slice()),
    );
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_name(agent_id),
        write_string32(name),
    );
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_url(agent_id),
        write_string32(url),
    );
    // Default perms: per_tx_limit=1_000, expires_at=0, flags=1 (allow asset 1)
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_perms(agent_id),
        pack_agent_perms(1_000, 0, 1),
    );
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_registered_at(agent_id),
        u64_to_u256(current_block_height),
    );

    Ok(InstructionResult::Success)
}

// ── AgentPay ──────────────────────────────────────────────────────────

fn exec_agent_pay(
    evm_state: &mut EvmState,
    sender: Address,
    payment: &call_protocol::instructions::AgentPayment,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    if !agent_exists(evm_state, payment.agent_id) {
        return Err(ConsensusError::InvalidBlock("agent not found".into()));
    }
    agent_check_owner(evm_state, payment.agent_id, sender)?;
    agent_check_permissions(
        evm_state,
        payment.agent_id,
        payment.asset_id,
        payment.amount,
        current_block_height,
    )?;

    // Deduct agent balance
    let balance = agent_get_balance(evm_state, payment.agent_id, payment.asset_id);
    let new_balance = balance
        .checked_sub(payment.amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("agent pay: insufficient balance".into()))?;
    agent_set_balance(evm_state, payment.agent_id, payment.asset_id, new_balance);

    // Credit recipient
    let to_slot = slot_balance(payment.asset_id, payment.to);
    let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
    let to_bal = to_bal
        .checked_add(payment.amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("agent pay: balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

    Ok(InstructionResult::Success)
}

// ── AgentBatchPay ─────────────────────────────────────────────────────

fn exec_agent_batch_pay(
    evm_state: &mut EvmState,
    sender: Address,
    payments: &[call_protocol::instructions::AgentPayment],
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    for payment in payments {
        exec_agent_pay(evm_state, sender, payment, current_block_height)?;
    }
    Ok(InstructionResult::Success)
}

// ── AgentCall ─────────────────────────────────────────────────────────

fn exec_agent_call(
    evm_state: &mut EvmState,
    sender: Address,
    agent_id: u64,
    target: Address,
    data: &[u8],
    current_block_height: u64,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    if !agent_exists(evm_state, agent_id) {
        return Err(ConsensusError::InvalidBlock("agent not found".into()));
    }
    agent_check_owner(evm_state, agent_id, sender)?;

    let perms = evm_state.get_storage(&AGENT_ADDRESS, slot_agent_perms(agent_id));
    let (_, expires_at, flags) = unpack_agent_perms(perms);
    if expires_at != 0 && current_block_height > expires_at {
        return Err(ConsensusError::InvalidBlock(
            "agent call: permissions expired".into(),
        ));
    }
    if (flags & 2) == 0 {
        return Err(ConsensusError::InvalidBlock(
            "agent call: protocol not allowed".into(),
        ));
    }

    let executor = evm_executor.ok_or_else(|| {
        ConsensusError::InvalidBlock("AgentCall: EVM executor required".into())
    })?;

    let tx = call_evm::EvmTransaction {
        caller: sender,
        nonce: 0,
        gas_limit: 1_000_000,
        gas_price: 0,
        to: Some(target),
        value: call_primitives::U256::ZERO,
        data: call_primitives::Bytes::from(data.to_vec()),
        chain_id: executor.chain_id,
    };

    match executor.execute_tx(tx, evm_state) {
        Ok(_) => Ok(InstructionResult::Success),
        Err(e) => Err(ConsensusError::InvalidBlock(format!("agent call: {e:?}"))),
    }
}

// ── AgentBridgeDeposit ────────────────────────────────────────────────

fn exec_agent_bridge_deposit(
    evm_state: &mut EvmState,
    sender: Address,
    agent_id: u64,
    asset_id: u64,
    amount: u128,
    _target_chain: u64,
    target_address: &[u8],
    current_block_height: u64,
    bridge_config: Option<&call_bridge::BridgeConfig>,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    if !agent_exists(evm_state, agent_id) {
        return Err(ConsensusError::InvalidBlock("agent not found".into()));
    }
    agent_check_owner(evm_state, agent_id, sender)?;
    agent_check_permissions(evm_state, agent_id, asset_id, amount, current_block_height)?;

    // Deduct agent balance
    let agent_bal = agent_get_balance(evm_state, agent_id, asset_id);
    let agent_bal = agent_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("agent bridge deposit: insufficient agent balance".into()))?;
    agent_set_balance(evm_state, agent_id, asset_id, agent_bal);

    // Deduct sender protocol balance
    let sender_slot = slot_balance(asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    let sender_bal = sender_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("agent bridge deposit: insufficient sender balance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

    // Bridge to EVM (reuse BridgeToEvm logic)
    let to = call_primitives::Address::from_slice(&target_address[..target_address.len().min(20)]);
    exec_bridge_to_evm(
        evm_state,
        sender,
        asset_id,
        to,
        amount,
        current_block_height,
        bridge_config,
        evm_executor,
    )
}

// ── GrantAgentBalance ─────────────────────────────────────────────────

fn exec_grant_agent_balance(
    evm_state: &mut EvmState,
    sender: Address,
    agent_id: u64,
    asset_id: u64,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    if !agent_exists(evm_state, agent_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "GrantAgentBalance: agent {agent_id} not found"
        )));
    }
    agent_check_owner(evm_state, agent_id, sender)?;

    // Deduct owner balance
    let owner_slot = slot_balance(asset_id, sender);
    let owner_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, owner_slot));
    let owner_bal = owner_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("GrantAgentBalance: insufficient balance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, owner_slot, u128_to_u256(owner_bal));

    // Credit agent balance
    let agent_bal = agent_get_balance(evm_state, agent_id, asset_id);
    let agent_bal = agent_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("GrantAgentBalance: agent balance overflow".into()))?;
    agent_set_balance(evm_state, agent_id, asset_id, agent_bal);

    Ok(InstructionResult::Success)
}

// ── RevokeAgentBalance ────────────────────────────────────────────────

fn exec_revoke_agent_balance(
    evm_state: &mut EvmState,
    sender: Address,
    agent_id: u64,
    asset_id: u64,
) -> Result<InstructionResult, ConsensusError> {
    if !agent_exists(evm_state, agent_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "RevokeAgentBalance: agent {agent_id} not found"
        )));
    }
    agent_check_owner(evm_state, agent_id, sender)?;

    agent_set_balance(evm_state, agent_id, asset_id, 0);
    Ok(InstructionResult::Success)
}

// ── UpdateCompliance ──────────────────────────────────────────────────

fn exec_update_compliance(
    evm_state: &mut EvmState,
    asset_id: u64,
    sender: Address,
    target: Address,
    status: call_protocol::instructions::ComplianceStatus,
) -> Result<InstructionResult, ConsensusError> {
    check_asset_active(evm_state, asset_id)?;

    // Verify sender is the asset issuer
    let issuer = u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")));
    if issuer != sender {
        return Err(ConsensusError::InvalidBlock(
            "UpdateCompliance: only asset issuer can update compliance".into(),
        ));
    }

    let policy_id = get_compliance_policy(evm_state, asset_id);
    let status_u8 = status as u8;
    evm_state.set_storage(
        COMPLIANCE_ADDRESS,
        slot_compliance(target, policy_id),
        U256::from(status_u8),
    );

    Ok(InstructionResult::Success)
}

// ── ValidatorStake ────────────────────────────────────────────────────

fn exec_validator_stake(
    evm_state: &mut EvmState,
    sender: Address,
    ed25519_pubkey: [u8; 32],
    self_stake: u128,
) -> Result<InstructionResult, ConsensusError> {
    if self_stake < MIN_SELF_STAKE {
        return Err(ConsensusError::InvalidBlock(format!(
            "validator stake: below minimum {MIN_SELF_STAKE}"
        )));
    }

    // Check sender hasn't already staked
    let existing_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if existing_id != 0 {
        return Err(ConsensusError::InvalidBlock(
            "validator stake: already staked".into(),
        ));
    }

    // Deduct CALL from sender, credit escrow
    let call_asset_id = call_protocol::CALL_ASSET_ID;
    let sender_slot = slot_balance(call_asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    let sender_bal = sender_bal
        .checked_sub(self_stake)
        .ok_or_else(|| ConsensusError::InvalidBlock("validator stake: insufficient balance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

    let escrow_slot = slot_balance(call_asset_id, STAKING_ESCROW);
    let escrow_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, escrow_slot));
    let escrow_bal = escrow_bal
        .checked_add(self_stake)
        .ok_or_else(|| ConsensusError::InvalidBlock("validator stake: escrow overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, escrow_slot, u128_to_u256(escrow_bal));

    // Register validator
    let count = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()));
    let validator_id = count + 1; // IDs start at 1
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_count(), u64_to_u256(validator_id));

    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_by_addr(sender), u64_to_u256(validator_id));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_addr(validator_id), address_to_u256(sender));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(sender), u128_to_u256(self_stake));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_pubkey(sender), U256::from_be_slice(&ed25519_pubkey));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(sender), U256::from(1u8)); // 1 = active

    Ok(InstructionResult::Success)
}

// ── ValidatorUnstake ──────────────────────────────────────────────────

fn exec_validator_unstake(
    evm_state: &mut EvmState,
    sender: Address,
    validator_id: u32,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    let _validator_id = validator_id as u64;

    // Look up validator by sender address
    let stored_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if stored_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "validator unstake: not a validator".into(),
        ));
    }

    let status = evm_state
        .get_storage(&VALIDATOR_ADDRESS, slot_validator_status(sender))
        .to_be_bytes::<32>()[31];
    if status != 1 {
        return Err(ConsensusError::InvalidBlock(
            "validator unstake: already unbonding".into(),
        ));
    }

    // Read stake amount
    let stake = u256_to_u128(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_stake(sender)));

    // Set status to unbonding (2)
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(sender), U256::from(2u8));

    // Record unbond height
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_validator_unbond_height(sender),
        u64_to_u256(current_block_height),
    );

    // Add to unbonding queue
    let unbonding_count = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_unbonding_count()));
    let unbonding_idx = unbonding_count;
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_unbonding_count(), u64_to_u256(unbonding_idx + 1));

    // Pack (validator_id u64 | amount u128) into a single U256
    let mut packed = [0u8; 32];
    packed[8..16].copy_from_slice(&stored_id.to_be_bytes());
    packed[16..32].copy_from_slice(&stake.to_be_bytes());
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_unbonding(unbonding_idx),
        U256::from_be_bytes::<32>(packed),
    );

    Ok(InstructionResult::Success)
}

// ── ValidatorClaimUnbonded ─────────────────────────────────────────────

fn exec_validator_claim_unbonded(
    evm_state: &mut EvmState,
    sender: Address,
    validator_id: u32,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    let _validator_id = validator_id as u64;

    // Look up validator by sender address
    let stored_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if stored_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "validator claim: not a validator".into(),
        ));
    }

    let status = evm_state
        .get_storage(&VALIDATOR_ADDRESS, slot_validator_status(sender))
        .to_be_bytes::<32>()[31];
    if status != 2 {
        return Err(ConsensusError::InvalidBlock(
            "validator claim: not unbonding".into(),
        ));
    }

    // Check unbonding period elapsed
    let unbond_height = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_unbond_height(sender)));
    if current_block_height < unbond_height + UNBONDING_PERIOD_BLOCKS {
        return Err(ConsensusError::InvalidBlock(
            "validator claim: unbonding period not elapsed".into(),
        ));
    }

    // Find unbonding request
    let unbonding_count = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_unbonding_count()));
    let mut found = false;
    let mut amount = 0u128;
    for i in 0..unbonding_count {
        let packed = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_unbonding(i)).to_be_bytes::<32>();
        let entry_id = u64::from_be_bytes(packed[8..16].try_into().unwrap());
        if entry_id == stored_id {
            amount = u128::from_be_bytes(packed[16..32].try_into().unwrap());
            found = true;
            break;
        }
    }
    if !found {
        return Err(ConsensusError::InvalidBlock(
            "validator claim: no unbonding request found".into(),
        ));
    }

    // Return stake from escrow to sender
    let call_asset_id = call_protocol::CALL_ASSET_ID;
    let escrow_slot = slot_balance(call_asset_id, STAKING_ESCROW);
    let escrow_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, escrow_slot));
    let escrow_bal = escrow_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("validator claim: escrow underflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, escrow_slot, u128_to_u256(escrow_bal));

    let sender_slot = slot_balance(call_asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    let sender_bal = sender_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("validator claim: balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

    // Clear validator state
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_by_addr(sender), U256::ZERO);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(sender), U256::ZERO);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(sender), U256::ZERO);

    Ok(InstructionResult::Success)
}

// ── OracleSubmit ──────────────────────────────────────────────────────

fn exec_oracle_submit(
    evm_state: &mut EvmState,
    sender: Address,
    asset_id: u64,
    price: u128,
    _block_number: u64,
    timestamp: u64,
    _signature: &[u8],
    _sources: &[String],
) -> Result<InstructionResult, ConsensusError> {
    // Verify sender is a registered validator
    let validator_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if validator_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "oracle: sender not a registered validator".into(),
        ));
    }

    // Write price data to ORACLE_ADDRESS
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"price"), u128_to_u256(price));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"ts"), u64_to_u256(timestamp));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"block"), u64_to_u256(_block_number));

    Ok(InstructionResult::Success)
}

// ── GovernanceSubmitProposal ──────────────────────────────────────────

fn exec_governance_submit_proposal(
    evm_state: &mut EvmState,
    sender: Address,
    _proposal_type: call_governance::ProposalType,
    title: String,
    description: String,
    execution_data: Vec<u8>,
) -> Result<InstructionResult, ConsensusError> {
    // Deduct proposal deposit
    let call_asset_id = call_protocol::CALL_ASSET_ID;
    let sender_slot = slot_balance(call_asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    if sender_bal < PROPOSAL_DEPOSIT {
        return Err(ConsensusError::InvalidBlock(
            "governance: insufficient balance for proposal deposit".into(),
        ));
    }
    evm_state.set_storage(
        ASSET_ADDRESS,
        sender_slot,
        u128_to_u256(sender_bal - PROPOSAL_DEPOSIT),
    );

    // Read and increment proposal count
    let count = u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal_count()));
    let proposal_id = count + 1;
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal_count(),
        u64_to_u256(proposal_id),
    );

    // Write proposal metadata
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"proposer"),
        address_to_u256(sender),
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"title"),
        write_string32(&title),
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"desc"),
        write_string32(&description),
    );
    // execution_data is too large for a single slot; store hash as placeholder
    let data_hash = call_crypto::keccak256(&execution_data);
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"data"),
        U256::from_be_slice(data_hash.as_slice()),
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"status"),
        U256::from(1u8), // 1 = Active
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"votes_for"),
        U256::ZERO,
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"votes_against"),
        U256::ZERO,
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"votes_abstain"),
        U256::ZERO,
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"deposit"),
        u128_to_u256(PROPOSAL_DEPOSIT),
    );

    Ok(InstructionResult::Success)
}

// ── GovernanceVote ────────────────────────────────────────────────────

fn exec_governance_vote(
    evm_state: &mut EvmState,
    sender: Address,
    proposal_id: u64,
    vote: call_governance::Vote,
) -> Result<InstructionResult, ConsensusError> {
    // Check proposal is active
    let status = evm_state
        .get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"))
        .to_be_bytes::<32>()[31];
    if status != 1 {
        return Err(ConsensusError::InvalidBlock(
            "governance: proposal not active".into(),
        ));
    }

    // Check voter hasn't already voted
    let voter_slot = slot_gov_voter(proposal_id, sender);
    let has_voted = evm_state.get_storage(&GOVERNANCE_ADDRESS, voter_slot);
    if has_voted != U256::ZERO {
        return Err(ConsensusError::InvalidBlock(
            "governance: already voted".into(),
        ));
    }

    // Record vote
    let vote_u8 = match vote {
        call_governance::Vote::Yes => 1u8,
        call_governance::Vote::No => 2u8,
        call_governance::Vote::Abstain => 3u8,
    };
    evm_state.set_storage(GOVERNANCE_ADDRESS, voter_slot, U256::from(vote_u8));

    // Update tally
    let tally_suffix: &[u8] = match vote {
        call_governance::Vote::Yes => b"votes_for",
        call_governance::Vote::No => b"votes_against",
        call_governance::Vote::Abstain => b"votes_abstain",
    };
    let tally = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, tally_suffix)));
    let tally = tally + 1;
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, tally_suffix),
        u128_to_u256(tally),
    );

    Ok(InstructionResult::Success)
}

// ── GovernanceQueue ───────────────────────────────────────────────────

fn exec_governance_queue(
    evm_state: &mut EvmState,
    _sender: Address,
    proposal_id: u64,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    // Check proposal is active
    let status = evm_state
        .get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"))
        .to_be_bytes::<32>()[31];
    if status != 1 {
        return Err(ConsensusError::InvalidBlock(
            "governance: proposal not active".into(),
        ));
    }

    // Check quorum (simplified: votes_for > total_votes * quorum_bps / 10000)
    let votes_for = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_for")));
    let votes_against = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_against")));
    let votes_abstain = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_abstain")));
    let total_votes = votes_for + votes_against + votes_abstain;

    if total_votes == 0 || votes_for * 10_000 < total_votes * GOV_QUORUM_BPS {
        return Err(ConsensusError::InvalidBlock(
            "governance: quorum not reached".into(),
        ));
    }

    // Update status to queued (2)
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"status"),
        U256::from(2u8),
    );
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"queued_at"),
        u64_to_u256(current_block_height),
    );

    Ok(InstructionResult::Success)
}

// ── GovernanceExecute ─────────────────────────────────────────────────

fn exec_governance_execute(
    evm_state: &mut EvmState,
    _sender: Address,
    proposal_id: u64,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    // Check proposal is queued
    let status = evm_state
        .get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"))
        .to_be_bytes::<32>()[31];
    if status != 2 {
        return Err(ConsensusError::InvalidBlock(
            "governance: proposal not queued".into(),
        ));
    }

    // Check timelock elapsed
    let queued_at = u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"queued_at")));
    if current_block_height < queued_at + GOV_TIMELOCK_BLOCKS {
        return Err(ConsensusError::InvalidBlock(
            "governance: timelock not elapsed".into(),
        ));
    }

    // Update status to executed (3)
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_proposal(proposal_id, b"status"),
        U256::from(3u8),
    );

    Ok(InstructionResult::Success)
}

// ── GovernanceEmergencyPause ──────────────────────────────────────────

fn exec_governance_emergency_pause(
    evm_state: &mut EvmState,
    sender: Address,
    reason: String,
) -> Result<InstructionResult, ConsensusError> {
    // Verify sender is a registered validator
    let validator_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if validator_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "governance: sender not a registered validator".into(),
        ));
    }

    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
    evm_state.set_storage(
        GOVERNANCE_ADDRESS,
        slot_gov_pause_reason(),
        write_string32(&reason),
    );

    Ok(InstructionResult::Success)
}

// ── GovernanceEmergencyResume ─────────────────────────────────────────

fn exec_governance_emergency_resume(
    evm_state: &mut EvmState,
    sender: Address,
) -> Result<InstructionResult, ConsensusError> {
    // Verify sender is a registered validator
    let validator_id = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(sender)));
    if validator_id == 0 {
        return Err(ConsensusError::InvalidBlock(
            "governance: sender not a registered validator".into(),
        ));
    }

    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::ZERO);
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_pause_reason(), U256::ZERO);

    Ok(InstructionResult::Success)
}

// ── Transfer ──────────────────────────────────────────────────────────

fn exec_transfer(
    evm_state: &mut EvmState,
    asset_id: u64,
    from: Address,
    to: Address,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    if amount == 0 {
        return Ok(InstructionResult::Success);
    }
    check_asset_active(evm_state, asset_id)?;

    let policy_id = get_compliance_policy(evm_state, asset_id);
    check_address_compliance(evm_state, &from, policy_id)?;
    check_address_compliance(evm_state, &to, policy_id)?;

    let from_slot = slot_balance(asset_id, from);
    let from_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, from_slot));
    let from_bal = from_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("insufficient balance".into()))?;

    let to_slot = slot_balance(asset_id, to);
    let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
    let to_bal = to_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("balance overflow".into()))?;

    evm_state.set_storage(ASSET_ADDRESS, from_slot, u128_to_u256(from_bal));
    evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

    Ok(InstructionResult::Success)
}

// ── BatchTransfer ─────────────────────────────────────────────────────

fn exec_batch_transfer(
    evm_state: &mut EvmState,
    asset_id: u64,
    from: Address,
    payments: &[call_protocol::instructions::PaymentEntry],
) -> Result<InstructionResult, ConsensusError> {
    check_asset_active(evm_state, asset_id)?;

    let policy_id = get_compliance_policy(evm_state, asset_id);
    check_address_compliance(evm_state, &from, policy_id)?;

    let from_slot = slot_balance(asset_id, from);
    let mut from_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, from_slot));

    for p in payments {
        let amount = p.amount;
        if amount == 0 {
            continue;
        }
        if let Some(m) = &p.memo {
            m.validate()
                .map_err(|e| ConsensusError::InvalidBlock(format!("memo: {e}")))?;
        }
        check_address_compliance(evm_state, &p.to, policy_id)?;
        from_bal = from_bal
            .checked_sub(amount)
            .ok_or_else(|| ConsensusError::InvalidBlock("insufficient balance".into()))?;

        let to_slot = slot_balance(asset_id, p.to);
        let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
        let to_bal = to_bal
            .checked_add(amount)
            .ok_or_else(|| ConsensusError::InvalidBlock("balance overflow".into()))?;
        evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));
    }

    evm_state.set_storage(ASSET_ADDRESS, from_slot, u128_to_u256(from_bal));
    Ok(InstructionResult::Success)
}

// ── Approve ───────────────────────────────────────────────────────────

fn exec_approve(
    evm_state: &mut EvmState,
    asset_id: u64,
    owner: Address,
    spender: Address,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    check_asset_active(evm_state, asset_id)?;

    let slot = slot_allowance(asset_id, owner, spender);
    evm_state.set_storage(ASSET_ADDRESS, slot, u128_to_u256(amount));
    Ok(InstructionResult::Success)
}

// ── TransferFrom ──────────────────────────────────────────────────────

fn exec_transfer_from(
    evm_state: &mut EvmState,
    asset_id: u64,
    from: Address,
    to: Address,
    spender: Address,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    if amount == 0 {
        return Ok(InstructionResult::Success);
    }
    check_asset_active(evm_state, asset_id)?;

    let policy_id = get_compliance_policy(evm_state, asset_id);
    check_address_compliance(evm_state, &spender, policy_id)?;
    check_address_compliance(evm_state, &from, policy_id)?;
    check_address_compliance(evm_state, &to, policy_id)?;

    // Check and spend allowance
    let allowance_slot = slot_allowance(asset_id, from, spender);
    let allowance = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, allowance_slot));
    let allowance = allowance
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("insufficient allowance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, allowance_slot, u128_to_u256(allowance));

    // Transfer balance
    let from_slot = slot_balance(asset_id, from);
    let from_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, from_slot));
    let from_bal = from_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("insufficient balance".into()))?;

    let to_slot = slot_balance(asset_id, to);
    let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
    let to_bal = to_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("balance overflow".into()))?;

    evm_state.set_storage(ASSET_ADDRESS, from_slot, u128_to_u256(from_bal));
    evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

    Ok(InstructionResult::Success)
}

// ── Mint ──────────────────────────────────────────────────────────────

fn exec_mint(
    evm_state: &mut EvmState,
    asset_id: u64,
    sender: Address,
    to: Address,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    if amount == 0 {
        return Ok(InstructionResult::Success);
    }
    check_asset_active(evm_state, asset_id)?;

    // Verify sender is the asset issuer
    let issuer = u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")));
    if issuer != sender {
        return Err(ConsensusError::InvalidBlock("not asset issuer".into()));
    }

    // Check supply cap
    let supply = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")));
    let max_supply = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply")));
    let new_supply = supply
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("supply overflow".into()))?;
    if max_supply > 0 && new_supply > max_supply {
        return Err(ConsensusError::InvalidBlock("max supply exceeded".into()));
    }

    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(new_supply));

    let to_slot = slot_balance(asset_id, to);
    let to_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, to_slot));
    let to_bal = to_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

    Ok(InstructionResult::Success)
}

// ── Burn ──────────────────────────────────────────────────────────────

fn exec_burn(
    evm_state: &mut EvmState,
    asset_id: u64,
    sender: Address,
    from: Address,
    amount: u128,
) -> Result<InstructionResult, ConsensusError> {
    if amount == 0 {
        return Ok(InstructionResult::Success);
    }
    check_asset_active(evm_state, asset_id)?;

    // If caller is not the owner, check allowance
    if sender != from {
        let allowance_slot = slot_allowance(asset_id, from, sender);
        let allowance = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, allowance_slot));
        let allowance = allowance
            .checked_sub(amount)
            .ok_or_else(|| ConsensusError::InvalidBlock("insufficient allowance".into()))?;
        evm_state.set_storage(ASSET_ADDRESS, allowance_slot, u128_to_u256(allowance));
    }

    // Verify sender is the asset issuer (same as precompile logic)
    let issuer = u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")));
    if issuer != sender {
        return Err(ConsensusError::InvalidBlock("not asset issuer".into()));
    }

    // Update supply
    let supply = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")));
    let new_supply = supply
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("supply underflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(new_supply));

    // Debit balance
    let from_slot = slot_balance(asset_id, from);
    let from_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, from_slot));
    let from_bal = from_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("insufficient balance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, from_slot, u128_to_u256(from_bal));

    Ok(InstructionResult::Success)
}

// ── Asset Registration ────────────────────────────────────────────────

fn exec_register_asset(
    evm_state: &mut EvmState,
    sender: Address,
    symbol: &str,
    name: &str,
    decimals: u8,
    max_supply: u128,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    let executor = evm_executor.ok_or_else(|| {
        ConsensusError::InvalidBlock("RegisterAsset requires EVM executor".into())
    })?;

    // 1. Collect asset registration fee
    let fee = call_governance::config::DEFAULT_ASSET_REGISTRATION_FEE;
    if fee > 0 {
        let sender_balance = read_balance(evm_state, call_protocol::CALL_ASSET_ID, sender);
        if sender_balance < fee {
            return Err(ConsensusError::InvalidBlock(format!(
                "RegisterAsset: insufficient CALL balance for fee: need {fee}, have {sender_balance}"
            )));
        }
        seed_balance(evm_state, call_protocol::CALL_ASSET_ID, sender, sender_balance - fee);
    }

    // 2. Read next asset ID from slot 0
    let next_id_slot = U256::ZERO;
    let next_id = u256_to_u64(evm_state.get_storage(&ASSET_ADDRESS, next_id_slot));
    let asset_id = if next_id == 0 { 1 } else { next_id };
    let next_id = asset_id.checked_add(1)
        .ok_or_else(|| ConsensusError::InvalidBlock("asset id overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, next_id_slot, u64_to_u256(next_id));

    // 3. Write asset metadata to EVM storage
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol"), write_string32(symbol));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"name"), write_string32(name));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals"), U256::from(decimals));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"), address_to_u256(sender));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply"), u128_to_u256(max_supply));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(0));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"), U256::from(0u8)); // Active
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"), U256::from(0u8));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at"), U256::from(0));

    // 4. Deploy EVM wrapped token via system deployer
    let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
    evm_state.create_account(deployer);

    let (contract_addr, deploy_result) = executor
        .deploy_erc20_template(
            deployer,
            evm_state,
            name,
            symbol,
            decimals,
            call_protocol::BRIDGE_EVM_ADDRESS,
            sender,
            call_primitives::U256::from(max_supply),
            call_primitives::U256::from(asset_id),
        )
        .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: ERC-20 deploy failed: {e:?}")))?;

    if !deploy_result.success {
        return Err(ConsensusError::InvalidBlock(
            "RegisterAsset: ERC-20 deployment reverted".into(),
        ));
    }

    // 5. Store contract address in BRIDGE_ADDRESS slot
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(asset_id), address_to_u256(contract_addr));

    Ok(InstructionResult::Success)
}

fn exec_evm_issuer_mint(
    evm_state: &mut EvmState,
    sender: Address,
    asset_id: u64,
    to: Address,
    amount: u128,
    evm_executor: Option<&call_evm::EvmExecutor>,
) -> Result<InstructionResult, ConsensusError> {
    let executor = evm_executor.ok_or_else(|| {
        ConsensusError::InvalidBlock("EvmIssuerMint requires EVM executor".into())
    })?;

    // 1. Asset must exist and be active
    let issuer = u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")));
    if issuer == Address::ZERO {
        return Err(ConsensusError::InvalidBlock(format!(
            "EvmIssuerMint: asset {asset_id} not found"
        )));
    }
    let status = read_asset_status(evm_state, asset_id);
    if status != 0 {
        return Err(ConsensusError::InvalidBlock(format!(
            "EvmIssuerMint: asset {asset_id} is not active (status: {status})"
        )));
    }

    // 2. Only issuer can mint
    if issuer != sender {
        return Err(ConsensusError::InvalidBlock(
            "EvmIssuerMint: caller is not asset issuer".into(),
        ));
    }

    // 3. CALL (asset_id == 1) has no wrapped ERC-20 contract
    if asset_id == call_protocol::CALL_ASSET_ID {
        return Err(ConsensusError::InvalidBlock(
            "EvmIssuerMint: CALL asset has no EVM wrapped token".into(),
        ));
    }

    // 4. Cap check
    let supply = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")));
    let max_supply = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply")));
    if max_supply > 0 && supply.saturating_add(amount) > max_supply {
        return Err(ConsensusError::InvalidBlock(format!(
            "EvmIssuerMint: cap exceeded for asset {asset_id}"
        )));
    }

    // 5. Must have an EVM contract address
    let contract = bridge_get_evm_contract(evm_state, asset_id).ok_or_else(|| {
        ConsensusError::InvalidBlock(format!(
            "EvmIssuerMint: no EVM contract registered for asset {asset_id}"
        ))
    })?;

    // 6. Execute EVM issuerMint
    let mint_result = executor
        .evm_call_issuer_mint(sender, contract, evm_state, to, call_primitives::U256::from(amount))
        .map_err(|e| {
            ConsensusError::InvalidBlock(format!("EvmIssuerMint: EVM call failed: {e:?}"))
        })?;

    if !mint_result.success {
        return Err(ConsensusError::InvalidBlock(
            "EvmIssuerMint: EVM issuerMint reverted".into(),
        ));
    }

    // 7. Update supply in EVM storage
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(supply + amount));

    Ok(InstructionResult::Success)
}

// ── Helpers for tests / genesis / RPC ─────────────────────────────────

/// Seed an asset balance directly into EVM storage.
pub fn seed_balance(evm_state: &mut EvmState, asset_id: u64, addr: Address, amount: u128) {
    evm_state.set_storage(ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

/// Seed asset metadata directly into EVM storage.
#[allow(clippy::too_many_arguments)]
pub fn seed_asset(
    evm_state: &mut EvmState,
    asset_id: u64,
    symbol: &str,
    name: &str,
    decimals: u8,
    issuer: Address,
    max_supply: u128,
    supply: u128,
    status: u8,
) {
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol"), write_string32(symbol));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"name"), write_string32(name));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals"), call_primitives::U256::from(decimals));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"), address_to_u256(issuer));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply"), u128_to_u256(max_supply));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(supply));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"), call_primitives::U256::from(status));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"), call_primitives::U256::from(0));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at"), call_primitives::U256::from(0));
}

/// Read an asset balance from EVM storage.
pub fn read_balance(evm_state: &EvmState, asset_id: u64, addr: Address) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_balance(asset_id, addr)))
}

/// Read asset info from EVM storage.
pub fn read_asset_symbol(evm_state: &EvmState, asset_id: u64) -> String {
    read_string32(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol")))
}

/// Read asset issuer from EVM storage.
pub fn read_asset_issuer(evm_state: &EvmState, asset_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")))
}

/// Read asset status from EVM storage.
pub fn read_asset_status(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"status")).to_be_bytes::<32>()[31]
}

/// Seed bridge contract address for an asset in EVM storage.
pub fn seed_bridge_contract(evm_state: &mut EvmState, asset_id: u64, contract: Address) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(asset_id), address_to_u256(contract));
}

/// Read total bridge deposits for an asset from EVM storage.
pub fn read_bridge_total_deposits(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_deposits(asset_id)))
}

/// Read total bridge withdrawals for an asset from EVM storage.
pub fn read_bridge_total_withdrawals(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_withdrawals(asset_id)))
}

// ── Asset read helpers ────────────────────────────────────────────────

/// Read asset name from EVM storage.
pub fn read_asset_name(evm_state: &EvmState, asset_id: u64) -> String {
    read_string32(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"name")))
}

/// Read asset decimals from EVM storage.
pub fn read_asset_decimals(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals")).to_be_bytes::<32>()[31]
}

/// Read asset supply from EVM storage.
pub fn read_asset_supply(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")))
}

/// Read asset max supply from EVM storage.
pub fn read_asset_max_supply(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply")))
}

/// Read asset compliance policy from EVM storage.
pub fn read_asset_compliance(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance")).to_be_bytes::<32>()[31]
}

/// Read asset registered_at from EVM storage.
pub fn read_asset_registered_at(evm_state: &EvmState, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at")))
}

// ── Agent read helpers ────────────────────────────────────────────────

/// Read agent name from EVM storage.
pub fn agent_get_name(evm_state: &EvmState, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_name(agent_id)))
}

/// Read agent url from EVM storage.
pub fn agent_get_url(evm_state: &EvmState, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_url(agent_id)))
}

/// Read agent registered_at from EVM storage.
pub fn agent_get_registered_at(evm_state: &EvmState, agent_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_registered_at(agent_id)))
}

// ── Validator read helpers ────────────────────────────────────────────

/// Read validator count from EVM storage.
pub fn read_validator_count(evm_state: &EvmState) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()))
}

/// Read validator address by validator ID from EVM storage.
pub fn read_validator_addr(evm_state: &EvmState, validator_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_addr(validator_id)))
}

/// Read validator stake from EVM storage.
pub fn read_validator_stake(evm_state: &EvmState, addr: Address) -> u128 {
    u256_to_u128(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_stake(addr)))
}

/// Read validator ed25519 pubkey from EVM storage.
pub fn read_validator_pubkey(evm_state: &EvmState, addr: Address) -> [u8; 32] {
    let pk_u256 = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_pubkey(addr));
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pk_u256.to_be_bytes::<32>());
    pk
}

/// Read validator status from EVM storage.
pub fn read_validator_status(evm_state: &EvmState, addr: Address) -> u8 {
    evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_status(addr)).to_be_bytes::<32>()[31]
}

/// Read validator ID by address from EVM storage.
pub fn read_validator_id_by_addr(evm_state: &EvmState, addr: Address) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(addr)))
}

/// Read all active validator addresses from EVM storage.
pub fn read_validator_addresses(evm_state: &EvmState) -> Vec<Address> {
    let count = read_validator_count(evm_state);
    let mut addrs = Vec::new();
    for i in 1..=count {
        let addr = read_validator_addr(evm_state, i);
        if addr != Address::ZERO && read_validator_status(evm_state, addr) != 0 {
            addrs.push(addr);
        }
    }
    addrs
}

// ── Governance read helpers ───────────────────────────────────────────

/// Read governance proposal status from EVM storage.
pub fn read_gov_proposal_status(evm_state: &EvmState, proposal_id: u64) -> u8 {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status")).to_be_bytes::<32>()[31]
}

/// Read governance paused flag from EVM storage.
pub fn read_gov_paused(evm_state: &EvmState) -> bool {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_paused()).to_be_bytes::<32>()[31] == 1
}

// ── Compliance read helpers ───────────────────────────────────────────

/// Read compliance status for an address under a policy from EVM storage.
pub fn read_compliance_status(evm_state: &EvmState, addr: Address, policy_id: u8) -> u8 {
    evm_state.get_storage(&COMPLIANCE_ADDRESS, slot_compliance(addr, policy_id)).to_be_bytes::<32>()[31]
}

// ── Oracle reward pool (stored in EVM storage) ────────────────────────

fn slot_oracle_reward_pool() -> U256 {
    storage_slot(&[b"oracle_reward_pool"])
}

/// Read the oracle reward pool from EVM storage.
pub fn read_oracle_reward_pool(evm_state: &EvmState) -> u128 {
    evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle_reward_pool())
        .to_be_bytes::<32>()[16..32]
        .try_into()
        .map(u128::from_be_bytes)
        .unwrap_or(0)
}

/// Add to the oracle reward pool in EVM storage.
pub fn add_oracle_reward(evm_state: &mut EvmState, amount: u128) {
    let current = read_oracle_reward_pool(evm_state);
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_reward_pool(), u128_to_u256(current + amount));
}

/// Seed validator state directly into EVM storage (for tests / genesis).
pub fn seed_validator(
    evm_state: &mut EvmState,
    validator_id: u64,
    addr: Address,
    ed25519_pubkey: [u8; 32],
    stake: u128,
    status: u8,
) {
    let count = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()));
    if validator_id > count {
        evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_count(), u64_to_u256(validator_id));
    }
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_addr(validator_id), address_to_u256(addr));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_by_addr(addr), u64_to_u256(validator_id));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(stake));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_pubkey(addr), U256::from_be_slice(&ed25519_pubkey));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(addr), U256::from(status));
}

/// Seed agent metadata directly into EVM storage (for tests / genesis).
pub fn seed_agent(
    evm_state: &mut EvmState,
    agent_id: u64,
    owner: Address,
    name: &str,
    url: &str,
    registered_at: u64,
) {
    let count = u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_count()));
    if agent_id >= count {
        evm_state.set_storage(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(agent_id + 1));
    }
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_owner(agent_id), address_to_u256(owner));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_name(agent_id), write_string32(name));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_url(agent_id), write_string32(url));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_registered_at(agent_id), u64_to_u256(registered_at));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_perms(agent_id), pack_agent_perms(1_000, 0, 1));
}

// ── Shielded storage slot helpers ─────────────────────────────────────

fn slot_shielded_merkle_root() -> U256 {
    storage_slot(&[b"merkle_root"])
}

fn slot_shielded_nullifier(nullifier: &call_shielded::Nullifier) -> U256 {
    storage_slot(&[b"nullifier", nullifier.as_ref()])
}

fn slot_shielded_commitment_count() -> U256 {
    storage_slot(&[b"cm_count"])
}

fn slot_shielded_commitment(index: u64) -> U256 {
    storage_slot(&[b"commitment", &index.to_be_bytes()[..]])
}

/// Sync a single commitment and the updated merkle root to EVM storage.
fn sync_shielded_deposit_to_evm(
    evm_state: &mut EvmState,
    shielded_state: &call_shielded::ShieldedState,
    commitment: &call_shielded::NoteCommitment,
) {
    let count = u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()));
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_commitment(count),
        U256::from_be_slice(commitment.as_ref()),
    );
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_commitment_count(),
        u64_to_u256(count + 1),
    );
    let root = shielded_state.merkle_root();
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_merkle_root(),
        U256::from_be_slice(root.as_slice()),
    );
}

/// Mark a nullifier as spent in EVM storage.
fn sync_shielded_nullifier_to_evm(
    evm_state: &mut EvmState,
    nullifier: &call_shielded::Nullifier,
) {
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_nullifier(nullifier),
        U256::from(1),
    );
}

// ── ShieldedDeposit ───────────────────────────────────────────────────

fn exec_shielded_deposit(
    evm_state: &mut EvmState,
    shielded_state: &mut ShieldedState,
    sender: Address,
    asset_id: u64,
    amount: u128,
    commitment: call_primitives::Hash,
    encrypted_note: &[u8],
) -> Result<InstructionResult, ConsensusError> {
    // Deduct transparent balance
    let sender_slot = slot_balance(asset_id, sender);
    let sender_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, sender_slot));
    let sender_bal = sender_bal
        .checked_sub(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("shielded deposit: insufficient balance".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

    // Reconstruct note
    let note = call_shielded::Note::from_encrypted_bytes(encrypted_note)
        .map_err(|e| ConsensusError::InvalidBlock(format!("shielded deposit: invalid encrypted note: {e}")))?;
    let note_cm = call_shielded::NoteCommitment::new(commitment);

    // Process in shielded state
    shielded_state
        .process_deposit(note_cm.clone(), note)
        .map_err(|e| ConsensusError::InvalidBlock(format!("shielded deposit: {e}")))?;

    // Sync to EVM storage
    sync_shielded_deposit_to_evm(evm_state, shielded_state, &note_cm);

    Ok(InstructionResult::Success)
}

// ── ShieldedWithdraw ──────────────────────────────────────────────────

fn exec_shielded_withdraw(
    evm_state: &mut EvmState,
    shielded_state: &mut ShieldedState,
    asset_id: u64,
    target: Address,
    amount: u128,
    proof: &[u8],
    nullifier: call_primitives::Hash,
) -> Result<InstructionResult, ConsensusError> {
    let zk_proof = call_shielded::ZkProof {
        proof_data: proof.to_vec(),
        nullifiers: vec![call_shielded::Nullifier::new(nullifier)],
        commitments: vec![],
        asset_id,
    };

    // Structural validation
    if !call_shielded::verify_zk_proof(&zk_proof) {
        return Err(ConsensusError::InvalidBlock(
            "shielded withdraw: invalid ZK proof".into(),
        ));
    }

    // Real Groth16 verification when real-prover feature is enabled
    #[cfg(feature = "real-prover")]
    {
        let merkle_root = shielded_state.merkle_root();
        let merkle_root_bytes: [u8; 32] = merkle_root.into();
        let valid = call_shielded::verify_shielded_proof(
            &zk_proof,
            "withdraw",
            Some(&merkle_root_bytes),
            Some(amount),
        )
        .map_err(|e| {
            ConsensusError::InvalidBlock(format!("shielded withdraw: proof verification error: {e}"))
        })?;
        if !valid {
            return Err(ConsensusError::InvalidBlock(
                "shielded withdraw: ZK proof verification failed".into(),
            ));
        }
    }

    shielded_state
        .process_withdraw(call_shielded::Nullifier::new(nullifier))
        .map_err(|e| ConsensusError::InvalidBlock(format!("shielded withdraw: {e}")))?;

    // Mark nullifier spent in EVM storage
    sync_shielded_nullifier_to_evm(evm_state, &call_shielded::Nullifier::new(nullifier));

    // Credit transparent balance
    let target_slot = slot_balance(asset_id, target);
    let target_bal = u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, target_slot));
    let target_bal = target_bal
        .checked_add(amount)
        .ok_or_else(|| ConsensusError::InvalidBlock("shielded withdraw: balance overflow".into()))?;
    evm_state.set_storage(ASSET_ADDRESS, target_slot, u128_to_u256(target_bal));

    Ok(InstructionResult::Success)
}

// ── ShieldedTransfer ──────────────────────────────────────────────────

fn exec_shielded_transfer(
    evm_state: &mut EvmState,
    shielded_state: &mut ShieldedState,
    asset_id: u64,
    proof: &[u8],
    nullifiers: &[call_primitives::Hash],
    commitments: &[call_primitives::Hash],
    encrypted_notes: &[Vec<u8>],
) -> Result<InstructionResult, ConsensusError> {
    let zk_proof = call_shielded::ZkProof {
        proof_data: proof.to_vec(),
        nullifiers: nullifiers.iter().map(|h| call_shielded::Nullifier::new(*h)).collect(),
        commitments: commitments.iter().map(|h| call_shielded::NoteCommitment::new(*h)).collect(),
        asset_id,
    };

    // Structural validation
    if !call_shielded::verify_zk_proof(&zk_proof) {
        return Err(ConsensusError::InvalidBlock(
            "shielded transfer: invalid ZK proof structure".into(),
        ));
    }

    // Real Groth16 verification when real-prover feature is enabled
    #[cfg(feature = "real-prover")]
    {
        let merkle_root = shielded_state.merkle_root();
        let merkle_root_bytes: [u8; 32] = merkle_root.into();
        let valid = call_shielded::verify_shielded_proof(
            &zk_proof,
            "transfer",
            Some(&merkle_root_bytes),
            None,
        )
        .map_err(|e| {
            ConsensusError::InvalidBlock(format!("shielded transfer: proof verification error: {e}"))
        })?;
        if !valid {
            return Err(ConsensusError::InvalidBlock(
                "shielded transfer: ZK proof verification failed".into(),
            ));
        }
    }

    // Decrypt output notes from encrypted_notes field
    let output_notes: Vec<call_shielded::Note> = encrypted_notes
        .iter()
        .filter_map(|data| call_shielded::Note::from_encrypted_bytes(data).ok())
        .collect();

    let transfer = call_shielded::ShieldedTransfer {
        input_notes: vec![], // input notes are not transmitted; proven via ZK
        output_notes,
        proof: zk_proof,
    };

    shielded_state
        .process_transfer(&transfer)
        .map_err(|e| ConsensusError::InvalidBlock(format!("shielded transfer: {e}")))?;

    // Sync nullifiers to EVM
    for nf in nullifiers {
        sync_shielded_nullifier_to_evm(evm_state, &call_shielded::Nullifier::new(*nf));
    }

    // Sync commitments to EVM
    for cm in commitments {
        let count = u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()));
        evm_state.set_storage(
            SHIELDED_ADDRESS,
            slot_shielded_commitment(count),
            U256::from_be_slice(cm.as_slice()),
        );
        evm_state.set_storage(
            SHIELDED_ADDRESS,
            slot_shielded_commitment_count(),
            u64_to_u256(count + 1),
        );
    }

    // Sync merkle root
    let root = shielded_state.merkle_root();
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_merkle_root(),
        U256::from_be_slice(root.as_slice()),
    );

    Ok(InstructionResult::Success)
}

// ── Shielded read helpers ─────────────────────────────────────────────

/// Read shielded merkle root from EVM storage.
pub fn read_shielded_merkle_root(evm_state: &EvmState) -> call_primitives::Hash {
    let root_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_merkle_root());
    call_primitives::Hash::from_slice(&root_u256.to_be_bytes::<32>())
}

/// Read shielded commitment count from EVM storage.
pub fn read_shielded_commitment_count(evm_state: &EvmState) -> u64 {
    u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()))
}

/// Read a shielded commitment by index from EVM storage.
pub fn read_shielded_commitment(evm_state: &EvmState, index: u64) -> call_primitives::Hash {
    let cm_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment(index));
    call_primitives::Hash::from_slice(&cm_u256.to_be_bytes::<32>())
}

/// Check if a nullifier is spent in EVM storage.
pub fn read_shielded_nullifier_spent(evm_state: &EvmState, nullifier: &call_shielded::Nullifier) -> bool {
    evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_nullifier(nullifier)).to_be_bytes::<32>()[31] == 1
}

/// Seed a shielded commitment directly into EVM storage (for tests / genesis).
pub fn seed_shielded_commitment(
    evm_state: &mut EvmState,
    index: u64,
    commitment: call_primitives::Hash,
) {
    let count = u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()));
    if index >= count {
        evm_state.set_storage(SHIELDED_ADDRESS, slot_shielded_commitment_count(), u64_to_u256(index + 1));
    }
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_commitment(index),
        U256::from_be_slice(commitment.as_slice()),
    );
}

/// Seed a shielded nullifier as spent directly into EVM storage (for tests / genesis).
pub fn seed_shielded_nullifier(evm_state: &mut EvmState, nullifier: &call_shielded::Nullifier) {
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_nullifier(nullifier),
        U256::from(1),
    );
}

//! Governance precompile at 0x203
//!
//! On-chain governance: submitProposal, vote, queue, execute,
//! emergencyPause, emergencyResume.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

use crate::{current_caller, state_hook};
use call_governance::{ProposalType, Vote};
use call_protocol::CALL_ASSET_ID;

#[allow(dead_code)]
pub(crate) const GOVERNANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000203");

// ── ABI decoding helpers ──────────────────────────────────────────────

fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

fn decode_u8(input: &[u8], slot_offset: usize) -> Option<u8> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    Some(input[slot_offset + 31])
}

fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    let val = u64::from_be_bytes(buf);
    Some(val as usize)
}

/// Decode a dynamic `string` or `bytes` type from ABI input.
/// `slot_offset` points to the 32-byte offset slot.
fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset; // args start at byte 4
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let data_start = abs_offset + 32;
    if input.len() < data_start + len {
        return None;
    }
    Some(input[data_start..data_start + len].to_vec())
}

fn decode_string(input: &[u8], slot_offset: usize) -> Option<String> {
    let bytes = decode_bytes(input, slot_offset)?;
    String::from_utf8(bytes).ok()
}

fn require_caller() -> Result<alloy_primitives::Address, PrecompileError> {
    current_caller()
        .ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

// ── Proposal type decoding ────────────────────────────────────────────

/// Decode a ProposalType from a uint8 discriminant + opaque executionData bytes.
///
/// Encoding per type:
/// 0 = ParameterChange: "param_id\0new_value" (null-delimited UTF-8)
/// 1 = ProtocolUpgrade: u64_be(activation_block) + changelog UTF-8
/// 2 = TreasurySpend: address(20) + u128_be(amount) + u64_be(asset_id)
/// 3 = ValidatorSlash: u32_be(validator_id) + reason UTF-8
/// 4 = ComplianceUpdate: u64_be(asset_id) + u8(new_policy)
/// 5 = EmergencyPause: reason UTF-8
fn decode_proposal_type(proposal_type_u8: u8, data: &[u8]) -> Option<ProposalType> {
    match proposal_type_u8 {
        0 => {
            // ParameterChange: null-delimited "param_id\0new_value"
            let (param_id, new_value) = split_at_null(data)?;
            Some(ProposalType::ParameterChange { param_id, new_value })
        }
        1 => {
            // ProtocolUpgrade: u64_be(activation_block) + changelog
            if data.len() < 8 {
                return None;
            }
            let activation_block = u64::from_be_bytes(data[..8].try_into().ok()?);
            let changelog = String::from_utf8(data[8..].to_vec()).ok()?;
            Some(ProposalType::ProtocolUpgrade {
                activation_block,
                changelog,
            })
        }
        2 => {
            // TreasurySpend: address(20) + u128_be(amount) + u64_be(asset_id)
            if data.len() < 44 {
                return None;
            }
            let recipient = alloy_primitives::Address::from_slice(&data[..20]);
            let amount = u128::from_be_bytes(data[20..36].try_into().ok()?);
            let asset_id = u64::from_be_bytes(data[36..44].try_into().ok()?);
            Some(ProposalType::TreasurySpend {
                recipient,
                amount,
                asset_id,
            })
        }
        3 => {
            // ValidatorSlash: u32_be(validator_id) + reason
            if data.len() < 4 {
                return None;
            }
            let validator_id = u32::from_be_bytes(data[..4].try_into().ok()?);
            let reason = String::from_utf8(data[4..].to_vec()).ok()?;
            Some(ProposalType::ValidatorSlash {
                validator_id,
                reason,
            })
        }
        4 => {
            // ComplianceUpdate: u64_be(asset_id) + u8(new_policy)
            if data.len() < 9 {
                return None;
            }
            let asset_id = u64::from_be_bytes(data[..8].try_into().ok()?);
            let new_policy = data[8];
            Some(ProposalType::ComplianceUpdate {
                asset_id,
                new_policy,
            })
        }
        5 => {
            // EmergencyPause: reason UTF-8
            let reason = String::from_utf8(data.to_vec()).ok()?;
            Some(ProposalType::EmergencyPause { reason })
        }
        _ => None,
    }
}

fn split_at_null(data: &[u8]) -> Option<(String, String)> {
    let pos = data.iter().position(|&b| b == 0)?;
    let first = String::from_utf8(data[..pos].to_vec()).ok()?;
    let second = String::from_utf8(data[pos + 1..].to_vec()).ok()?;
    Some((first, second))
}

fn decode_vote(vote_u8: u8) -> Option<Vote> {
    match vote_u8 {
        0 => Some(Vote::Yes),
        1 => Some(Vote::No),
        2 => Some(Vote::Abstain),
        _ => None,
    }
}

// ── Governance precompile entry point ─────────────────────────────────

pub fn governance_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0x58, 0x30, 0xd3, 0xea] => submit_proposal(input, gas_limit),
        &[0xb0, 0x40, 0xd1, 0x66] => vote(input, gas_limit),
        &[0x92, 0x6c, 0x46, 0xb2] => queue(input, gas_limit),
        &[0xb5, 0x90, 0xbe, 0x77] => execute(input, gas_limit),
        &[0xcf, 0x5b, 0x4f, 0xd5] => emergency_pause(input, gas_limit),
        &[0x93, 0xc8, 0x7f, 0x03] => emergency_resume(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── submitProposal(uint8 proposalType, string title, string description, bytes executionData) -> uint64

fn submit_proposal(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 50000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let proposal_type_u8 = decode_u8(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid proposal_type".into())
    })?;
    let title = decode_string(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid title".into())
    })?;
    let description = decode_string(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid description".into())
    })?;
    let execution_data = decode_bytes(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid execution_data".into())
    })?;

    let proposal_type = decode_proposal_type(proposal_type_u8, &execution_data)
        .ok_or_else(|| PrecompileError::Other("invalid proposal type or execution_data".into()))?;

    let caller = require_caller()?;

    // Deduct proposal deposit from caller's CALL balance
    let deposit = state_hook::with_governance(|gov| gov.config.proposal_deposit)
        .ok_or_else(|| PrecompileError::Other("governance not available".into()))?;

    let has_balance = state_hook::with_account_state(|acc| {
        acc.get_balance(CALL_ASSET_ID, &caller) >= deposit
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))?;

    if !has_balance {
        return Err(PrecompileError::Other("insufficient balance for proposal deposit".into()));
    }

    state_hook::with_account_state(|acc| {
        acc.deduct_balance(CALL_ASSET_ID, caller, deposit)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    let proposal_id = state_hook::with_governance(|gov| {
        gov.submit_proposal_with_deposit(
            caller,
            proposal_type,
            title,
            description,
            execution_data,
        )
        .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    // Encode proposal_id as uint256 (32 bytes)
    let mut output = [0u8; 32];
    output[24..].copy_from_slice(&proposal_id.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── vote(uint64 proposalId, uint8 vote)

fn vote(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let proposal_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid proposal_id".into())
    })?;
    let vote_u8 = decode_u8(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid vote".into())
    })?;
    let vote = decode_vote(vote_u8)
        .ok_or_else(|| PrecompileError::Other("invalid vote value".into()))?;

    let caller = require_caller()?;

    state_hook::with_governance(|gov| {
        gov.vote(proposal_id, caller, vote)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── queue(uint64 proposalId)

fn queue(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 15000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let proposal_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid proposal_id".into())
    })?;

    state_hook::with_governance(|gov| {
        gov.queue_proposal(proposal_id)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── execute(uint64 proposalId)

fn execute(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 30000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let proposal_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid proposal_id".into())
    })?;

    let caller = require_caller()?;

    state_hook::with_governance(|gov| {
        gov.execute_proposal(proposal_id, caller)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── emergencyPause(string reason)

fn emergency_pause(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let reason = decode_string(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid reason".into())
    })?;

    let caller = require_caller()?;

    state_hook::with_governance(|gov| {
        let validator_id = gov
            .validator_id_by_address(caller)
            .ok_or_else(|| PrecompileError::Other("caller is not a registered validator".into()))?;
        gov.emergency_pause_initiate(validator_id, reason)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── emergencyResume()

fn emergency_resume(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    // No input args to decode; ignore any trailing data
    let _ = input;

    state_hook::with_governance(|gov| {
        gov.emergency_pause_resume()
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("governance not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_governance::GovernanceManager;
    use call_primitives::Address;
    use call_protocol::{AccountState, AssetRegistry};
    use call_protocol::compliance::ComplianceEngine;
    use call_shielded::ShieldedState;
    use call_oracle::OracleManager;

    fn setup_state_hook(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        governance: &mut GovernanceManager,
    ) -> crate::state_hook::StateHookGuard {
        let mut shielded = ShieldedState::new();
        let mut oracle = OracleManager::default();

        crate::state_hook::StateHookGuard::new(
            account,
            registry,
            compliance,
            &mut shielded,
            Some(&mut oracle),
            Some(governance),
        )
    }

    fn encode_submit_proposal(
        proposal_type: u8,
        title: &str,
        description: &str,
        execution_data: &[u8],
    ) -> Vec<u8> {
        // ABI encode: submitProposal(uint8, string, string, bytes)
        // selector + proposalType(32) + title_offset(32) + desc_offset(32) + data_offset(32)
        // + title_len(32) + title_data(padded)
        // + desc_len(32) + desc_data(padded)
        // + data_len(32) + data_data(padded)
        let mut input = vec![0u8; 132];
        input[0..4].copy_from_slice(&[0x58, 0x30, 0xd3, 0xea]);
        input[35] = proposal_type;

        let title_bytes = title.as_bytes();
        let desc_bytes = description.as_bytes();

        // Offset values are relative to args start (byte 4)
        let title_offset: u64 = 128;
        let title_padded_len = ((title_bytes.len() + 31) / 32) * 32;
        let desc_offset: u64 = 128 + 32 + title_padded_len as u64;
        let desc_padded_len = ((desc_bytes.len() + 31) / 32) * 32;
        let data_offset: u64 = desc_offset + 32 + desc_padded_len as u64;

        // Offsets are uint256 values in the last 8 bytes of each 32-byte slot
        input[60..68].copy_from_slice(&title_offset.to_be_bytes());
        input[92..100].copy_from_slice(&desc_offset.to_be_bytes());
        input[124..132].copy_from_slice(&data_offset.to_be_bytes());

        // Append title: 32-byte uint256 length + padded data
        {
            let mut len_buf = [0u8; 32];
            len_buf[24..].copy_from_slice(&(title_bytes.len() as u64).to_be_bytes());
            input.extend_from_slice(&len_buf);
        }
        input.extend_from_slice(title_bytes);
        let title_padding = title_padded_len - title_bytes.len();
        input.extend(std::iter::repeat(0u8).take(title_padding));

        // Append description: 32-byte uint256 length + padded data
        {
            let mut len_buf = [0u8; 32];
            len_buf[24..].copy_from_slice(&(desc_bytes.len() as u64).to_be_bytes());
            input.extend_from_slice(&len_buf);
        }
        input.extend_from_slice(desc_bytes);
        let desc_padding = desc_padded_len - desc_bytes.len();
        input.extend(std::iter::repeat(0u8).take(desc_padding));

        // Append executionData: 32-byte uint256 length + padded data
        {
            let mut len_buf = [0u8; 32];
            len_buf[24..].copy_from_slice(&(execution_data.len() as u64).to_be_bytes());
            input.extend_from_slice(&len_buf);
        }
        input.extend_from_slice(execution_data);
        let data_padding = ((execution_data.len() + 31) / 32) * 32 - execution_data.len();
        input.extend(std::iter::repeat(0u8).take(data_padding));

        input
    }

    #[test]
    fn test_governance_address() {
        assert_eq!(
            GOVERNANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000203")
        );
    }

    #[test]
    fn test_submit_proposal() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        // Fund caller with enough CALL for deposit
        account.credit_balance(CALL_ASSET_ID, caller, 100_000 * 10u128.pow(18)).unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        // Encode: ParameterChange proposal
        let exec_data = b"max_block_size\x0010000000";
        let input = encode_submit_proposal(0, "Increase block size", "Double it", exec_data);

        let result = governance_precompile_fn(&input, 100000);
        assert!(result.is_ok(), "submitProposal failed: {:?}", result);

        // Verify proposal was created
        let proposal = governance.get_proposal(0).expect("proposal should exist");
        assert_eq!(proposal.title, "Increase block size");
        assert_eq!(proposal.proposer, caller);
        assert!(matches!(proposal.proposal_type, ProposalType::ParameterChange { .. }));

        // Deposit deducted
        let balance = account.get_balance(CALL_ASSET_ID, &caller);
        assert_eq!(balance, 100_000 * 10u128.pow(18) - governance.config.proposal_deposit);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_submit_proposal_insufficient_balance() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        // Don't fund caller

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        let exec_data = b"max_block_size\x0010000000";
        let input = encode_submit_proposal(0, "Title", "Desc", exec_data);

        let result = governance_precompile_fn(&input, 100000);
        assert!(result.is_err());

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_vote() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        account.credit_balance(CALL_ASSET_ID, caller, 100_000 * 10u128.pow(18)).unwrap();

        // Register caller as validator for voting power
        governance.register_validator(1, caller);
        governance.set_call_balance(caller, 100_000 * 10u128.pow(18));
        // Short review period so voting starts immediately
        governance.config.review_period_blocks = 0;

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        // Submit proposal first
        let exec_data = b"max_block_size\x0010000000";
        let input = encode_submit_proposal(0, "Title", "Desc", exec_data);
        let result = governance_precompile_fn(&input, 100000);
        assert!(result.is_ok(), "submit failed: {:?}", result);

        // Advance block so voting starts
        governance.set_current_block(10);

        // Vote yes
        let mut vote_input = vec![0u8; 68];
        vote_input[0..4].copy_from_slice(&[0xb0, 0x40, 0xd1, 0x66]);
        vote_input[28..36].copy_from_slice(&0u64.to_be_bytes());
        vote_input[67] = 0; // Yes

        let result = governance_precompile_fn(&vote_input, 50000);
        assert!(result.is_ok(), "vote failed: {:?}", result);

        // Verify vote counted
        let proposal = governance.get_proposal(0).unwrap();
        assert!(proposal.voting_power_yes > 0);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_queue_and_execute() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        account.credit_balance(CALL_ASSET_ID, caller, 100_000 * 10u128.pow(18)).unwrap();

        governance.register_validator(1, caller);
        governance.set_call_balance(caller, 100_000 * 10u128.pow(18));
        // Short periods for testing
        governance.config.review_period_blocks = 0;
        governance.config.voting_period_blocks = 1;
        governance.config.timelock_period_blocks = 0;
        governance.config.execution_timeout_blocks = 100;

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        // Submit proposal
        let exec_data = b"max_block_size\x0010000000";
        let input = encode_submit_proposal(0, "Title", "Desc", exec_data);
        governance_precompile_fn(&input, 100000).unwrap();

        // Vote yes (block 1, voting starts at block 0 + review_period = 0)
        governance.set_current_block(1);
        let mut vote_input = vec![0u8; 68];
        vote_input[0..4].copy_from_slice(&[0xb0, 0x40, 0xd1, 0x66]);
        vote_input[28..36].copy_from_slice(&0u64.to_be_bytes());
        vote_input[67] = 0;
        governance_precompile_fn(&vote_input, 50000).unwrap();

        // Advance past voting period
        governance.set_current_block(5);

        // Queue
        let mut queue_input = vec![0u8; 36];
        queue_input[0..4].copy_from_slice(&[0x92, 0x6c, 0x46, 0xb2]);
        queue_input[28..36].copy_from_slice(&0u64.to_be_bytes());
        let result = governance_precompile_fn(&queue_input, 50000);
        assert!(result.is_ok(), "queue failed: {:?}", result);

        // Execute
        let mut exec_input = vec![0u8; 36];
        exec_input[0..4].copy_from_slice(&[0xb5, 0x90, 0xbe, 0x77]);
        exec_input[28..36].copy_from_slice(&0u64.to_be_bytes());
        let result = governance_precompile_fn(&exec_input, 50000);
        assert!(result.is_ok(), "execute failed: {:?}", result);

        // Verify executed
        let proposal = governance.get_proposal(0).unwrap();
        assert_eq!(proposal.state, call_governance::ProposalState::Executed);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_emergency_pause() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        governance.register_validator(1, caller);

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        // ABI encode: emergencyPause(string reason)
        let reason = "security incident";
        let reason_bytes = reason.as_bytes();
        let reason_padded = ((reason_bytes.len() + 31) / 32) * 32;
        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0xcf, 0x5b, 0x4f, 0xd5]);
        // offset to string data = 32 (relative to args start)
        input[28..36].copy_from_slice(&32u64.to_be_bytes());
        // Append 32-byte length + padded data
        {
            let mut len_buf = [0u8; 32];
            len_buf[24..].copy_from_slice(&(reason_bytes.len() as u64).to_be_bytes());
            input.extend_from_slice(&len_buf);
        }
        input.extend_from_slice(reason_bytes);
        input.extend(std::iter::repeat(0u8).take(reason_padded - reason_bytes.len()));

        let result = governance_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "emergencyPause failed: {:?}", result);
        assert!(governance.is_paused());
        assert_eq!(governance.emergency_pause.pause_reason, "security incident");

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_emergency_resume() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        governance.register_validator(1, caller);

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);

        // First pause via direct API
        governance.emergency_pause_initiate(1, "test".into()).unwrap();
        assert!(governance.is_paused());

        // Then resume via precompile
        let input = vec![0x93, 0xc8, 0x7f, 0x03];
        let result = governance_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "emergencyResume failed: {:?}", result);
        assert!(!governance.is_paused());
    }

    #[test]
    fn test_emergency_pause_not_validator() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut governance = GovernanceManager::new();

        let caller = Address::repeat_byte(0x11);
        // NOT registered as validator

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut governance);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        let reason = "security incident";
        let reason_bytes = reason.as_bytes();
        let reason_padded = ((reason_bytes.len() + 31) / 32) * 32;
        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0xcf, 0x5b, 0x4f, 0xd5]);
        input[28..36].copy_from_slice(&32u64.to_be_bytes());
        {
            let mut len_buf = [0u8; 32];
            len_buf[24..].copy_from_slice(&(reason_bytes.len() as u64).to_be_bytes());
            input.extend_from_slice(&len_buf);
        }
        input.extend_from_slice(reason_bytes);
        input.extend(std::iter::repeat(0u8).take(reason_padded - reason_bytes.len()));

        let result = governance_precompile_fn(&input, 50000);
        assert!(result.is_err());

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }
}

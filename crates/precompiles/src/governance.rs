//! Governance precompile at 0x203
//!
//! Functions: submitProposal, vote, queue, execute, emergencyPause, emergencyResume,
//!            getProposalStatus, getProposalVotes, isPaused, getProposalCount

use alloy_primitives::{address, Address, U256};
use revm_precompile::PrecompileError;

use crate::StatefulPrecompile;
use crate::storage::{storage_slot, StorageCtx};
use crate::{
    address_to_u256, decode_bytes32, decode_u64, decode_u8, encode_u64,
    encode_u8, load_bal, ok_empty, require_validator, save_bal, slot_balance,
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256,
    u8_to_u256,
};

pub const GOVERNANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000203");

const CALL_ASSET_ID: u64 = 1;
const PROPOSAL_DEPOSIT: u128 = 10_000;
const GOV_TIMELOCK_BLOCKS: u64 = 100;
const GOV_QUORUM_BPS: u128 = 3_333;

// ── Storage slot helpers ──────────────────────────────────────────────

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

// ── Governance helpers ────────────────────────────────────────────────

fn proposal_status(proposal_id: u64) -> u8 {
    StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"))
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(0)
}

fn require_proposal_status(proposal_id: u64, expected: u8, err: &str) -> Result<(), PrecompileError> {
    if proposal_status(proposal_id) != expected {
        return Err(PrecompileError::Other(err.to_string().into()));
    }
    Ok(())
}

fn vote_tally(proposal_id: u64, suffix: &[u8]) -> u128 {
    StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn increment_tally(proposal_id: u64, suffix: &[u8]) {
    let tally = vote_tally(proposal_id, suffix);
    StorageCtx::sstore(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix), u128_to_u256(tally + 1));
}

// ── GovernancePrecompile ──────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct GovernancePrecompile;

impl GovernancePrecompile {
    // submitProposal(bytes32 title, bytes32 description, bytes32 dataHash) -> 0x13169e1c
    fn submit_proposal(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 200_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let title = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid title".into()))?;
        let description = decode_bytes32(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid description".into()))?;
        let data_hash = decode_bytes32(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid dataHash".into()))?;

        // Deduct proposal deposit
        let sender_slot = slot_balance(CALL_ASSET_ID, msg_sender);
        let sender_bal = StorageCtx::sload(crate::ASSET_ADDRESS, sender_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        if sender_bal < PROPOSAL_DEPOSIT {
            return Err(PrecompileError::Other(
                "governance: insufficient balance for proposal deposit".into(),
            ));
        }
        StorageCtx::sstore(
            crate::ASSET_ADDRESS,
            sender_slot,
            u128_to_u256(sender_bal - PROPOSAL_DEPOSIT),
        );

        // Increment proposal count
        let count = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        let proposal_id = count + 1;
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_count(),
            u64_to_u256(proposal_id),
        );

        // Write proposal metadata
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
            address_to_u256(msg_sender),
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"title"),
            U256::from_be_slice(&title),
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"desc"),
            U256::from_be_slice(&description),
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"data"),
            U256::from_be_slice(&data_hash),
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(1u8), // 1 = Active
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_for"),
            U256::ZERO,
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_against"),
            U256::ZERO,
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_abstain"),
            U256::ZERO,
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"deposit"),
            u128_to_u256(PROPOSAL_DEPOSIT),
        );

        // Emit ProposalSubmitted(proposalId, proposer)
        let topic0 = alloy_primitives::keccak256(b"ProposalSubmitted(uint64,address)");
        let mut event_data = Vec::with_capacity(64);
        event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
        event_data.extend_from_slice(&address_to_u256(msg_sender).to_be_bytes::<32>());
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // vote(uint64 proposalId, uint8 vote) -> 0x023d033b
    fn vote(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 10_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let proposal_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid proposalId".into()))?;
        let vote_val = decode_u8(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid vote".into()))?;

        if vote_val < 1 || vote_val > 3 {
            return Err(PrecompileError::Other("governance: invalid vote value".into()));
        }

        require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        // Check voter hasn't already voted
        let voter_slot = slot_gov_voter(proposal_id, msg_sender);
        let has_voted = StorageCtx::sload(GOVERNANCE_ADDRESS, voter_slot)
            .unwrap_or(U256::ZERO);
        if has_voted != U256::ZERO {
            return Err(PrecompileError::Other("governance: already voted".into()));
        }

        // Record vote
        StorageCtx::sstore(GOVERNANCE_ADDRESS, voter_slot, U256::from(vote_val));

        // Update tally
        let tally_suffix: &[u8] = match vote_val {
            1 => b"votes_for",
            2 => b"votes_against",
            3 => b"votes_abstain",
            _ => unreachable!(),
        };
        increment_tally(proposal_id, tally_suffix);

        // Emit VoteCast(proposalId, voter, vote)
        let topic0 = alloy_primitives::keccak256(b"VoteCast(uint64,address,uint8)");
        let mut event_data = Vec::with_capacity(96);
        event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
        event_data.extend_from_slice(&address_to_u256(msg_sender).to_be_bytes::<32>());
        event_data.extend_from_slice(&u8_to_u256(vote_val).to_be_bytes::<32>());
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // queue(uint64 proposalId) -> 0xfe72d010
    fn queue(
        &self,
        input: &[u8],
        _msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let proposal_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid proposalId".into()))?;

        require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        // Check quorum and majority
        let votes_for = vote_tally(proposal_id, b"votes_for");
        let votes_against = vote_tally(proposal_id, b"votes_against");
        let votes_abstain = vote_tally(proposal_id, b"votes_abstain");
        let total_votes = votes_for + votes_against + votes_abstain;

        if total_votes == 0 || votes_for * 10_000 < total_votes * GOV_QUORUM_BPS {
            return Err(PrecompileError::Other("governance: quorum not reached".into()));
        }
        if votes_for <= votes_against {
            return Err(PrecompileError::Other("governance: not enough for votes".into()));
        }

        // Update status to queued (2)
        let current_block = StorageCtx::block_number();
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(2u8),
        );
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"queued_at"),
            u64_to_u256(current_block),
        );

        // Emit ProposalQueued(proposalId, queuedAt)
        let topic0 = alloy_primitives::keccak256(b"ProposalQueued(uint64,uint64)");
        let mut event_data = Vec::with_capacity(64);
        event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
        event_data.extend_from_slice(&u64_to_u256(current_block).to_be_bytes::<32>());
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // execute(uint64 proposalId) -> 0x50f701f4
    fn execute(
        &self,
        input: &[u8],
        _msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let proposal_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid proposalId".into()))?;

        require_proposal_status(proposal_id, 2, "governance: proposal not queued")?;

        // Check timelock elapsed
        let queued_at = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"queued_at"))
            .map(u256_to_u64)
            .unwrap_or(0);
        let current_block = StorageCtx::block_number();
        if current_block < queued_at + GOV_TIMELOCK_BLOCKS {
            return Err(PrecompileError::Other(
                "governance: timelock not elapsed".into(),
            ));
        }

        // Refund deposit to proposer
        let proposer = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"proposer"))
            .map(u256_to_address)
            .unwrap_or(Address::ZERO);
        let deposit = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"deposit"))
            .map(u256_to_u128)
            .unwrap_or(0);
        if deposit > 0 && proposer != Address::ZERO {
            let bal = load_bal(CALL_ASSET_ID, proposer);
            save_bal(CALL_ASSET_ID, proposer, bal + deposit);
            StorageCtx::sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"deposit"),
                U256::ZERO,
            );
        }

        // Update status to executed (3)
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(3u8),
        );

        // Emit ProposalExecuted(proposalId)
        let topic0 = alloy_primitives::keccak256(b"ProposalExecuted(uint64)");
        let mut event_data = Vec::with_capacity(32);
        event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // emergencyPause(bytes32 reason) -> 0x7b391c64
    fn emergency_pause(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let reason = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid reason".into()))?;

        require_validator(msg_sender)?;

        StorageCtx::sstore(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
        StorageCtx::sstore(
            GOVERNANCE_ADDRESS,
            slot_gov_pause_reason(),
            U256::from_be_slice(&reason),
        );

        // Emit EmergencyPaused(pauser, reason)
        let topic0 = alloy_primitives::keccak256(b"EmergencyPaused(address,bytes32)");
        let mut event_data = Vec::with_capacity(64);
        event_data.extend_from_slice(&address_to_u256(msg_sender).to_be_bytes::<32>());
        event_data.extend_from_slice(&reason);
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // emergencyResume() -> 0x597c1a8d
    fn emergency_resume(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20_000;
        if StorageCtx::is_static() {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        // No args beyond selector
        if input.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        require_validator(msg_sender)?;

        StorageCtx::sstore(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::ZERO);
        StorageCtx::sstore(GOVERNANCE_ADDRESS, slot_gov_pause_reason(), U256::ZERO);

        // Emit EmergencyResumed(resumer)
        let topic0 = alloy_primitives::keccak256(b"EmergencyResumed(address)");
        let mut event_data = Vec::with_capacity(32);
        event_data.extend_from_slice(&address_to_u256(msg_sender).to_be_bytes::<32>());
        if let Some(log) = alloy_primitives::LogData::new(vec![topic0], alloy_primitives::Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
        }

        ok_empty()
    }

    // getProposalStatus(uint64 proposalId) -> uint8 -> 0x7d62d795
    fn get_proposal_status(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let proposal_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid proposalId".into()))?;

        let status = proposal_status(proposal_id);

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, encode_u8(status).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getProposalVotes(uint64 proposalId) -> (uint128,uint128,uint128) -> 0xeb795856
    fn get_proposal_votes(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let proposal_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid proposalId".into()))?;

        let votes_for = vote_tally(proposal_id, b"votes_for");
        let votes_against = vote_tally(proposal_id, b"votes_against");
        let votes_abstain = vote_tally(proposal_id, b"votes_abstain");

        let mut out = vec![0u8; 96];
        out[16..32].copy_from_slice(&votes_for.to_be_bytes());
        out[48..64].copy_from_slice(&votes_against.to_be_bytes());
        out[80..96].copy_from_slice(&votes_abstain.to_be_bytes());
        let out = revm_precompile::PrecompileOutput::new(GAS_COST, alloy_primitives::Bytes::from(out));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // isPaused() -> bool -> 0x2dfe3874
    fn is_paused(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let paused = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_paused())
            .map(|v| v.to_be_bytes::<32>()[31] == 1)
            .unwrap_or(false);

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, encode_u8(if paused { 1 } else { 0 }).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getProposalCount() -> uint64 -> 0x96ce4373
    fn get_proposal_count(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let count = StorageCtx::sload(GOVERNANCE_ADDRESS, slot_gov_proposal_count())
            .map(u256_to_u64)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, encode_u64(count).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for GovernancePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector = [calldata[0], calldata[1], calldata[2], calldata[3]];
        match selector {
            [0x13, 0x16, 0x9e, 0x1c] => self.submit_proposal(calldata, msg_sender),
            [0x02, 0x3d, 0x03, 0x3b] => self.vote(calldata, msg_sender),
            [0xfe, 0x72, 0xd0, 0x10] => self.queue(calldata, msg_sender),
            [0x50, 0xf7, 0x01, 0xf4] => self.execute(calldata, msg_sender),
            [0x7b, 0x39, 0x1c, 0x64] => self.emergency_pause(calldata, msg_sender),
            [0x59, 0x7c, 0x1a, 0x8d] => self.emergency_resume(calldata, msg_sender),
            [0x7d, 0x62, 0xd7, 0x95] => self.get_proposal_status(calldata),
            [0xeb, 0x79, 0x58, 0x56] => self.get_proposal_votes(calldata),
            [0x2d, 0xfe, 0x38, 0x74] => self.is_paused(calldata),
            [0x96, 0xce, 0x43, 0x73] => self.get_proposal_count(calldata),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governance_address() {
        assert_eq!(
            GOVERNANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000203")
        );
    }

    #[test]
    fn test_governance_precompile_submit_and_get() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(100_000),
            );

            let mut precompile = GovernancePrecompile;

            // submitProposal(title, description, dataHash)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x13, 0x16, 0x9e, 0x1c]);
            input[4..36].copy_from_slice(b"My Proposal_____________________");
            input[36..68].copy_from_slice(b"Description_____________________");
            input[68..100].copy_from_slice(&[0xDDu8; 32]);

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "submit failed: {:?}", result.err());

            // getProposalCount
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x96, 0xce, 0x43, 0x73]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let count = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&result.bytes[24..32]);
                buf
            });
            assert_eq!(count, 1);

            // getProposalStatus(1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x7d, 0x62, 0xd7, 0x95]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1); // active

            // getProposalVotes(1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xeb, 0x79, 0x58, 0x56]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[16..32], &[0u8; 16]); // votes_for = 0
            assert_eq!(&result.bytes[48..64], &[0u8; 16]); // votes_against = 0
            assert_eq!(&result.bytes[80..96], &[0u8; 16]); // votes_abstain = 0
        });
    }

    #[test]
    fn test_governance_precompile_vote_queue_execute() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(100_000),
            );

            let mut precompile = GovernancePrecompile;

            // submitProposal
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x13, 0x16, 0x9e, 0x1c]);
            input[4..36].copy_from_slice(b"Proposal________________________");
            input[36..68].copy_from_slice(b"Desc____________________________");
            input[68..100].copy_from_slice(&[0xEEu8; 32]);
            precompile.call(&input, sender).unwrap();

            // vote(proposalId=1, vote=1=Yes)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x02, 0x3d, 0x03, 0x3b]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            // uint8 at slot_offset 36 occupies byte 36+31 = 67
            input[67] = 1;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "vote failed: {:?}", result.err());

            // getProposalVotes(1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xeb, 0x79, 0x58, 0x56]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let votes_for = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(votes_for, 1);

            // queue(1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xfe, 0x72, 0xd0, 0x10]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "queue failed: {:?}", result.err());

            // getProposalStatus(1) should be 2 (queued)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x7d, 0x62, 0xd7, 0x95]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 2);
        });
    }

    #[test]
    fn test_governance_precompile_emergency_pause_and_resume() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed validator so pause/resume work
            crate::storage::StorageCtx::sstore(
                crate::VALIDATOR_ADDRESS,
                crate::slot_validator_by_addr(sender),
                u64_to_u256(1),
            );

            let mut precompile = GovernancePrecompile;

            // isPaused() -> false
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x2d, 0xfe, 0x38, 0x74]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);

            // emergencyPause(reason)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x7b, 0x39, 0x1c, 0x64]);
            input[4..36].copy_from_slice(b"Emergency reason________________");
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "pause failed: {:?}", result.err());

            // isPaused() -> true
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x2d, 0xfe, 0x38, 0x74]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);

            // emergencyResume()
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x59, 0x7c, 0x1a, 0x8d]);
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "resume failed: {:?}", result.err());

            // isPaused() -> false
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x2d, 0xfe, 0x38, 0x74]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);
        });
    }
}

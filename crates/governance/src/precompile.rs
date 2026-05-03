//! Governance precompile entry point (0x203).
//!
//! Thin wrapper that routes EVM calls to [`GovernanceStorage`] backed by
//! [`JournalBackend`].  Business logic lives in [`GovernanceStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompiles::{
    address_to_u256, dispatch, journal_backend::JournalBackend, require_caller,
    storage::storage_slot, u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64,
    u64_to_u256, ASSET_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const GOVERNANCE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000203");

pub const CALL_ASSET_ID: u64 = 1;
pub const PROPOSAL_DEPOSIT: u128 = 10_000;
pub const GOV_TIMELOCK_BLOCKS: u64 = 100;
pub const GOV_QUORUM_BPS: u128 = 3_333;

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

// ── GovernanceStorage ─────────────────────────────────────────────────

/// Business logic for governance operations backed by any StorageBackend.
pub struct GovernanceStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> GovernanceStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn read_proposal_count(&self) -> u64 {
        u256_to_u64(self.backend.load(GOVERNANCE_ADDRESS, slot_gov_proposal_count()))
    }

    pub fn read_proposal_status(&self, proposal_id: u64) -> u8 {
        self.backend
            .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"))
            .to_be_bytes::<32>()[31]
    }

    pub fn require_proposal_status(
        &self,
        proposal_id: u64,
        expected: u8,
        err: &str,
    ) -> Result<(), PrecompileError> {
        if self.read_proposal_status(proposal_id) != expected {
            return Err(PrecompileError::Other(err.to_string().into()));
        }
        Ok(())
    }

    pub fn read_vote_tally(&self, proposal_id: u64, suffix: &[u8]) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix)),
        )
    }

    pub fn increment_tally(&mut self, proposal_id: u64, suffix: &[u8]) {
        let tally = self.read_vote_tally(proposal_id, suffix);
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, suffix),
            u128_to_u256(tally + 1),
        );
    }

    pub fn is_paused(&self) -> bool {
        self.backend
            .load(GOVERNANCE_ADDRESS, slot_gov_paused())
            .to_be_bytes::<32>()[31]
            != 0
    }

    pub fn submit_proposal(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        title: [u8; 32],
        description: [u8; 32],
        data_hash: [u8; 32],
        proposer: Address,
    ) -> Result<u64, PrecompileError> {
        asset_store
            .deduct_balance(CALL_ASSET_ID, proposer, PROPOSAL_DEPOSIT)
            .map_err(|_| {
                PrecompileError::Other(
                    "governance: insufficient balance for proposal deposit".into(),
                )
            })?;

        let count = self.read_proposal_count();
        let proposal_id = count + 1;
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_count(),
            u64_to_u256(proposal_id),
        );

        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
            address_to_u256(proposer),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"title"),
            U256::from_be_slice(&title),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"desc"),
            U256::from_be_slice(&description),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"data"),
            U256::from_be_slice(&data_hash),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(1u8),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_for"),
            U256::ZERO,
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_against"),
            U256::ZERO,
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"votes_abstain"),
            U256::ZERO,
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"deposit"),
            u128_to_u256(PROPOSAL_DEPOSIT),
        );

        Ok(proposal_id)
    }

    pub fn vote(
        &mut self,
        proposal_id: u64,
        vote_val: u8,
        voter: Address,
    ) -> Result<(), PrecompileError> {
        if vote_val < 1 || vote_val > 3 {
            return Err(PrecompileError::Other(
                "governance: invalid vote value".into(),
            ));
        }

        self.require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        let voter_slot = slot_gov_voter(proposal_id, voter);
        let has_voted = self.backend.load(GOVERNANCE_ADDRESS, voter_slot);
        if has_voted != U256::ZERO {
            return Err(PrecompileError::Other("governance: already voted".into()));
        }

        self.backend
            .store(GOVERNANCE_ADDRESS, voter_slot, U256::from(vote_val));

        let tally_suffix: &[u8] = match vote_val {
            1 => b"votes_for",
            2 => b"votes_against",
            3 => b"votes_abstain",
            _ => unreachable!(),
        };
        self.increment_tally(proposal_id, tally_suffix);

        Ok(())
    }

    pub fn queue(
        &mut self,
        proposal_id: u64,
        current_block: u64,
    ) -> Result<(), PrecompileError> {
        self.require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        let votes_for = self.read_vote_tally(proposal_id, b"votes_for");
        let votes_against = self.read_vote_tally(proposal_id, b"votes_against");
        let votes_abstain = self.read_vote_tally(proposal_id, b"votes_abstain");
        let total_votes = votes_for + votes_against + votes_abstain;

        if total_votes == 0 || votes_for * 10_000 < total_votes * GOV_QUORUM_BPS {
            return Err(PrecompileError::Other("governance: quorum not reached".into()));
        }
        if votes_for <= votes_against {
            return Err(PrecompileError::Other(
                "governance: not enough for votes".into(),
            ));
        }

        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(2u8),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"queued_at"),
            u64_to_u256(current_block),
        );

        Ok(())
    }

    pub fn execute(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        proposal_id: u64,
        current_block: u64,
    ) -> Result<(), PrecompileError> {
        self.require_proposal_status(proposal_id, 2, "governance: proposal not queued")?;

        let queued_at = u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"queued_at")),
        );
        if current_block < queued_at + GOV_TIMELOCK_BLOCKS {
            return Err(PrecompileError::Other(
                "governance: timelock not elapsed".into(),
            ));
        }

        let proposer = u256_to_address(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"proposer")),
        );
        let deposit = u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"deposit")),
        );
        if deposit > 0 && proposer != Address::ZERO {
            asset_store
                .add_balance(CALL_ASSET_ID, proposer, deposit)
                .map_err(|_| PrecompileError::Other("governance: refund failed".into()))?;
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"deposit"),
                U256::ZERO,
            );
        }

        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(3u8),
        );

        Ok(())
    }

    pub fn emergency_pause(&mut self, reason: [u8; 32], _pauser: Address) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_paused(),
            U256::from(1u8),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_pause_reason(),
            U256::from_be_slice(&reason),
        );
    }

    pub fn emergency_resume(&mut self) {
        self.backend.store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::ZERO);
        self.backend.store(GOVERNANCE_ADDRESS, slot_gov_pause_reason(), U256::ZERO);
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolGovernance {
        function submitProposal(bytes32 title, bytes32 description, bytes32 dataHash) external;
        function vote(uint64 proposalId, uint8 vote) external;
        function queue(uint64 proposalId) external;
        function execute(uint64 proposalId) external;
        function emergencyPause(bytes32 reason) external;
        function emergencyResume() external;
        function getProposalStatus(uint64 proposalId) external view returns (uint8);
        function getProposalVotes(uint64 proposalId) external view returns (uint128 votesFor, uint128 votesAgainst, uint128 votesAbstain);
        function isPaused() external view returns (uint8);
        function getProposalCount() external view returns (uint64);
    }
}

// ── GovernancePrecompile ──────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct GovernancePrecompile;

impl GovernancePrecompile {
    fn submit_proposal(&self,
        calldata: &[u8],
        msg_sender: Address,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::submitProposalCall, _>(calldata, 200_000, |call| {
            let caller = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut gov_store = GovernanceStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            let proposal_id = gov_store
                .submit_proposal(
                    &mut asset_store,
                    call.title.into(),
                    call.description.into(),
                    call.dataHash.into(),
                    caller,
                )
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

            // Emit ProposalSubmitted(proposalId, proposer)
            let topic0 = alloy_primitives::keccak256(b"ProposalSubmitted(uint64,address)");
            let mut event_data = Vec::with_capacity(64);
            event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
            event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn vote(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::voteCall, _>(calldata, 10_000, |call| {
            let caller = require_caller(msg_sender)?;
            let mut gov_store = GovernanceStorage::new(JournalBackend);
            gov_store
                .vote(call.proposalId, call.vote, caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

            // Emit VoteCast(proposalId, voter, vote)
            let topic0 = alloy_primitives::keccak256(b"VoteCast(uint64,address,uint8)");
            let mut event_data = Vec::with_capacity(96);
            event_data.extend_from_slice(&u64_to_u256(call.proposalId).to_be_bytes::<32>());
            event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
            event_data.extend_from_slice(&U256::from(call.vote).to_be_bytes::<32>());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn queue(&self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::queueCall, _>(calldata, 20_000, |call| {
            let mut gov_store = GovernanceStorage::new(JournalBackend);
            let block_number = call_precompiles::storage::StorageCtx::block_number();
            gov_store
                .queue(call.proposalId, block_number)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

            // Emit ProposalQueued(proposalId, queuedAt)
            let topic0 = alloy_primitives::keccak256(b"ProposalQueued(uint64,uint64)");
            let mut event_data = Vec::with_capacity(64);
            event_data.extend_from_slice(&u64_to_u256(call.proposalId).to_be_bytes::<32>());
            event_data.extend_from_slice(&u64_to_u256(block_number).to_be_bytes::<32>());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn execute(&self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::executeCall, _>(calldata, 20_000, |call| {
            let backend = JournalBackend;
            let mut gov_store = GovernanceStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            let block_number = call_precompiles::storage::StorageCtx::block_number();
            gov_store
                .execute(&mut asset_store, call.proposalId, block_number)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

            // Emit ProposalExecuted(proposalId)
            let topic0 = alloy_primitives::keccak256(b"ProposalExecuted(uint64)");
            let mut event_data = Vec::with_capacity(32);
            event_data.extend_from_slice(&u64_to_u256(call.proposalId).to_be_bytes::<32>());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn emergency_pause(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::emergencyPauseCall, _>(calldata, 30_000, |call| {
            let caller = require_caller(msg_sender)?;

            // Verify caller is a registered validator
            let validator_store = ValidatorStorage::new(JournalBackend);
            let validator_id = validator_store.read_validator_id(caller);
            if validator_id == 0 {
                return Err(PrecompileError::Other(
                    "sender not a registered validator".into(),
                ));
            }

            let mut gov_store = GovernanceStorage::new(JournalBackend);
            gov_store.emergency_pause(call.reason.into(), caller);

            // Emit EmergencyPaused(pauser, reason)
            let topic0 = alloy_primitives::keccak256(b"EmergencyPaused(address,bytes32)");
            let mut event_data = Vec::with_capacity(64);
            event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
            event_data.extend_from_slice(&call.reason.as_ref());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn emergency_resume(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::emergencyResumeCall, _>(calldata, 20_000, |_call| {
            let caller = require_caller(msg_sender)?;

            // Verify caller is a registered validator
            let validator_store = ValidatorStorage::new(JournalBackend);
            let validator_id = validator_store.read_validator_id(caller);
            if validator_id == 0 {
                return Err(PrecompileError::Other(
                    "sender not a registered validator".into(),
                ));
            }

            let mut gov_store = GovernanceStorage::new(JournalBackend);
            gov_store.emergency_resume();

            // Emit EmergencyResumed(resumer)
            let topic0 = alloy_primitives::keccak256(b"EmergencyResumed(address)");
            let mut event_data = Vec::with_capacity(32);
            event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
            if let Some(log) = alloy_primitives::LogData::new(
                vec![topic0],
                alloy_primitives::Bytes::from(event_data),
            ) {
                let _ = call_precompiles::storage::StorageCtx::emit_event(GOVERNANCE_ADDRESS, log);
            }

            Ok(())
        })
    }

    fn get_proposal_status(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalStatusCall, _, _>(calldata, 2000, |call| {
            let store = GovernanceStorage::new(JournalBackend);
            Ok(U256::from(store.read_proposal_status(call.proposalId)))
        })
    }

    fn get_proposal_votes(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalVotesCall, _, _>(calldata, 2000, |call| {
            let store = GovernanceStorage::new(JournalBackend);
            let votes_for = store.read_vote_tally(call.proposalId, b"votes_for");
            let votes_against = store.read_vote_tally(call.proposalId, b"votes_against");
            let votes_abstain = store.read_vote_tally(call.proposalId, b"votes_abstain");
            Ok((votes_for, votes_against, votes_abstain))
        })
    }

    fn is_paused(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::isPausedCall, _, _>(calldata, 1000, |_call| {
            let store = GovernanceStorage::new(JournalBackend);
            Ok(U256::from(if store.is_paused() { 1u8 } else { 0u8 }))
        })
    }

    fn get_proposal_count(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalCountCall, _, _>(calldata, 1000, |_call| {
            let store = GovernanceStorage::new(JournalBackend);
            Ok(store.read_proposal_count())
        })
    }
}

impl call_precompiles::StatefulPrecompile for GovernancePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        match selector {
            IProtocolGovernance::submitProposalCall::SELECTOR => {
                self.submit_proposal(calldata, msg_sender)
            }
            IProtocolGovernance::voteCall::SELECTOR => self.vote(calldata, msg_sender),
            IProtocolGovernance::queueCall::SELECTOR => self.queue(calldata, msg_sender),
            IProtocolGovernance::executeCall::SELECTOR => self.execute(calldata, msg_sender),
            IProtocolGovernance::emergencyPauseCall::SELECTOR => {
                self.emergency_pause(calldata, msg_sender)
            }
            IProtocolGovernance::emergencyResumeCall::SELECTOR => {
                self.emergency_resume(calldata, msg_sender)
            }
            IProtocolGovernance::getProposalStatusCall::SELECTOR => self.get_proposal_status(calldata),
            IProtocolGovernance::getProposalVotesCall::SELECTOR => self.get_proposal_votes(calldata),
            IProtocolGovernance::isPausedCall::SELECTOR => self.is_paused(calldata),
            IProtocolGovernance::getProposalCountCall::SELECTOR => {
                self.get_proposal_count(calldata)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompiles::storage::HashMapStorageProvider;
    use call_precompiles::{slot_balance, slot_validator_by_addr, u128_to_u256, u64_to_u256, StatefulPrecompile};
    use call_precompiles::{ASSET_ADDRESS, VALIDATOR_ADDRESS};

    #[test]
    fn test_governance_address() {
        assert_eq!(
            GOVERNANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000203")
        );
    }

    #[test]
    fn test_governance_precompile_submit_and_get() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(100_000),
            );

            let mut precompile = GovernancePrecompile;

            // submitProposal
            let input = IProtocolGovernance::submitProposalCall {
                title: alloy_primitives::FixedBytes::<32>::from_slice(b"My Proposal_____________________"),
                description: alloy_primitives::FixedBytes::<32>::from_slice(b"Description_____________________"),
                dataHash: alloy_primitives::FixedBytes::<32>::from_slice(&[0xDDu8; 32]),
            }
            .abi_encode();
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "submit failed: {:?}", result.err());

            // getProposalCount
            let input = IProtocolGovernance::getProposalCountCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let count = u256_to_u64(U256::from_be_bytes::<32>(result.bytes.as_ref().try_into().unwrap()));
            assert_eq!(count, 1);

            // getProposalStatus(1)
            let input = IProtocolGovernance::getProposalStatusCall { proposalId: 1 }.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1); // active

            // getProposalVotes(1)
            let input = IProtocolGovernance::getProposalVotesCall { proposalId: 1 }.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[16..32], &[0u8; 16]); // votes_for = 0
            assert_eq!(&result.bytes[48..64], &[0u8; 16]); // votes_against = 0
            assert_eq!(&result.bytes[80..96], &[0u8; 16]); // votes_abstain = 0
        });
    }

    #[test]
    fn test_governance_precompile_vote_queue_execute() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(100_000),
            );

            let mut precompile = GovernancePrecompile;

            // submitProposal
            let input = IProtocolGovernance::submitProposalCall {
                title: alloy_primitives::FixedBytes::<32>::from_slice(b"Proposal________________________"),
                description: alloy_primitives::FixedBytes::<32>::from_slice(b"Desc____________________________"),
                dataHash: alloy_primitives::FixedBytes::<32>::from_slice(&[0xEEu8; 32]),
            }
            .abi_encode();
            precompile.call(&input, sender).unwrap();

            // vote(proposalId=1, vote=1=Yes)
            let input = IProtocolGovernance::voteCall {
                proposalId: 1,
                vote: 1,
            }
            .abi_encode();
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "vote failed: {:?}", result.err());

            // getProposalVotes(1)
            let input = IProtocolGovernance::getProposalVotesCall { proposalId: 1 }.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let votes_for = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(votes_for, 1);

            // queue(1)
            let input = IProtocolGovernance::queueCall { proposalId: 1 }.abi_encode();
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "queue failed: {:?}", result.err());

            // getProposalStatus(1) should be 2 (queued)
            let input = IProtocolGovernance::getProposalStatusCall { proposalId: 1 }.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 2);
        });
    }

    #[test]
    fn test_governance_precompile_emergency_pause_and_resume() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed validator so pause/resume work
            call_precompiles::storage::StorageCtx::sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(sender),
                u64_to_u256(1),
            );

            let mut precompile = GovernancePrecompile;

            // isPaused() -> false
            let input = IProtocolGovernance::isPausedCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);

            // emergencyPause(reason)
            let input = IProtocolGovernance::emergencyPauseCall {
                reason: alloy_primitives::FixedBytes::<32>::from_slice(b"Emergency reason________________"),
            }
            .abi_encode();
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "pause failed: {:?}", result.err());

            // isPaused() -> true
            let input = IProtocolGovernance::isPausedCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);

            // emergencyResume()
            let input = IProtocolGovernance::emergencyResumeCall {}.abi_encode();
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "resume failed: {:?}", result.err());

            // isPaused() -> false
            let input = IProtocolGovernance::isPausedCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);
        });
    }
}

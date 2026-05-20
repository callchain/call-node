//! Governance precompile entry point (0x203).
//!
//! Thin wrapper that routes EVM calls to [`GovernanceStorage`] backed by
//! any [`StorageProvider`].  Business logic lives in [`GovernanceStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, check_compliance, dispatch, require_caller, slot_compliance,
    storage::{storage_slot, StorageProvider},
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256, StorageRef,
    COMPLIANCE_ADDRESS, VALIDATOR_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const GOVERNANCE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000203");

pub const CALL_ASSET_ID: u64 = 1;
pub const PROPOSAL_DEPOSIT: u128 = crate::config::DEFAULT_PROPOSAL_DEPOSIT;
pub const TREASURY_ADDRESS: Address = Address::repeat_byte(0xAA);
pub const GOV_TIMELOCK_BLOCKS: u64 = 100;
pub const GOV_QUORUM_BPS: u128 = 3_333;

// Governance config defaults (mirrors GovernanceConfig in manager.rs)
pub const GOV_REVIEW_PERIOD: u64 = 10;
pub const GOV_VOTING_PERIOD: u64 = 100;
pub const GOV_EXEC_TIMEOUT: u64 = 1000;
pub const GOV_PROPOSAL_COOLDOWN: u64 = 345_600;

/// Allowed governance parameter keys for ParameterChange proposals.
const ALLOWED_GOVERNANCE_PARAMS: &[&str] = &[
    "review_period",
    "voting_period",
    "timelock",
    "execution_timeout",
    "proposal_cooldown",
    "validator_quorum_bps",
    "supply_quorum_bps",
    "treasury_quorum_bps",
    "simple_majority_bps",
    "emergency_pause_bps",
];

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_gov_proposal_count() -> U256 {
    U256::ZERO
}

pub(crate) fn slot_gov_proposal(proposal_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], suffix])
}

fn slot_gov_voter(proposal_id: u64, voter: Address) -> U256 {
    storage_slot(&[b"vote", &proposal_id.to_be_bytes()[..], voter.as_slice()])
}

fn slot_gov_voter_vote(proposal_id: u64, voter: Address) -> U256 {
    storage_slot(&[b"vote_choice", &proposal_id.to_be_bytes()[..], voter.as_slice()])
}

fn slot_gov_paused() -> U256 {
    storage_slot(&[b"paused"])
}

fn slot_gov_pause_reason() -> U256 {
    storage_slot(&[b"pause_reason"])
}

// Config slots
fn slot_gov_config(suffix: &[u8]) -> U256 {
    storage_slot(&[b"gov_config", suffix])
}

fn slot_gov_proposal_review_end(proposal_id: u64) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], b"review_end"])
}

fn slot_gov_proposal_quorum_required(proposal_id: u64) -> U256 {
    storage_slot(&[
        b"proposal",
        &proposal_id.to_be_bytes()[..],
        b"quorum_required",
    ])
}

fn slot_gov_last_submission(addr: Address) -> U256 {
    storage_slot(&[b"last_submit", addr.as_slice()])
}

fn slot_gov_proposal_data_len(proposal_id: u64) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], b"data_len"])
}

fn slot_gov_proposal_data_chunk(proposal_id: u64, chunk_idx: u64) -> U256 {
    storage_slot(&[
        b"proposal",
        &proposal_id.to_be_bytes()[..],
        b"data_chunk",
        &chunk_idx.to_be_bytes()[..],
    ])
}

// Emergency multi-sig slots
fn slot_emergency_pause_epoch() -> U256 {
    storage_slot(&[b"emergency_pause_epoch"])
}

fn slot_emergency_pause_sig_epoch(addr: Address) -> U256 {
    storage_slot(&[b"emergency_pause_sig", addr.as_slice()])
}

fn slot_emergency_pause_sig_count() -> U256 {
    storage_slot(&[b"emergency_pause_sig_count"])
}

fn slot_emergency_resume_epoch() -> U256 {
    storage_slot(&[b"emergency_resume_epoch"])
}

fn slot_emergency_resume_sig_epoch(addr: Address) -> U256 {
    storage_slot(&[b"emergency_resume_sig", addr.as_slice()])
}

fn slot_emergency_resume_sig_count() -> U256 {
    storage_slot(&[b"emergency_resume_sig_count"])
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

    pub fn read_proposal_count(&mut self) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal_count()),
        )
    }

    pub fn read_proposal_status(&mut self, proposal_id: u64) -> u8 {
        self.backend
            .load(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"status"),
            )
            .to_be_bytes::<32>()[31]
    }

    pub fn read_proposal_u64(&mut self, proposal_id: u64, suffix: &[u8]) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix)),
        )
    }

    pub fn read_proposal_u8(&mut self, proposal_id: u64, suffix: &[u8]) -> u8 {
        self.backend
            .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix))
            .to_be_bytes::<32>()[31]
    }

    pub fn read_proposal_u128(&mut self, proposal_id: u64, suffix: &[u8]) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix)),
        )
    }

    pub fn read_proposal_proposer(&mut self, proposal_id: u64) -> Address {
        u256_to_address(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
        ))
    }

    /// Maximum execution_data size (1 MB) to prevent OOM from malicious data_len.
    const MAX_EXECUTION_DATA_LEN: usize = 1024 * 1024;

    /// Read the stored execution_data length without loading chunks.
    pub fn read_proposal_data_len(&mut self, proposal_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal_data_len(proposal_id)),
        )
    }

    /// Read back the execution_data stored in chunked slots.
    pub fn read_proposal_execution_data(&mut self, proposal_id: u64) -> Vec<u8> {
        let data_len = u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal_data_len(proposal_id)),
        ) as usize;
        if data_len == 0 {
            return Vec::new();
        }
        if data_len > Self::MAX_EXECUTION_DATA_LEN {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(data_len);
        let num_chunks = (data_len + 31) / 32;
        for chunk_idx in 0..num_chunks {
            let chunk = self
                .backend
                .load(
                    GOVERNANCE_ADDRESS,
                    slot_gov_proposal_data_chunk(proposal_id, chunk_idx as u64),
                )
                .to_be_bytes::<32>();
            let remaining = data_len - result.len();
            result.extend_from_slice(&chunk[..remaining.min(32)]);
        }
        result
    }

    pub fn require_proposal_status(
        &mut self,
        proposal_id: u64,
        expected: u8,
        err: &str,
    ) -> Result<(), PrecompileError> {
        if self.read_proposal_status(proposal_id) != expected {
            return Err(PrecompileError::Other(err.to_string().into()));
        }
        Ok(())
    }

    /// Validate that a proposal ID is within the valid range.
    pub fn require_valid_proposal_id(&mut self, proposal_id: u64) -> Result<(), PrecompileError> {
        if proposal_id == 0 || proposal_id > self.read_proposal_count() {
            return Err(PrecompileError::Other(
                "governance: invalid proposal id".into(),
            ));
        }
        Ok(())
    }

    pub fn read_vote_tally(&mut self, proposal_id: u64, suffix: &[u8]) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix)),
        )
    }

    pub fn read_voter_power(&mut self, proposal_id: u64, voter: Address) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_voter(proposal_id, voter)),
        )
    }

    pub fn read_voter_vote(&mut self, proposal_id: u64, voter: Address) -> u8 {
        self.backend
            .load(GOVERNANCE_ADDRESS, slot_gov_voter_vote(proposal_id, voter))
            .to_be_bytes::<32>()[31]
    }

    pub fn read_validator_count(&mut self) -> u64 {
        self.backend
            .load(VALIDATOR_ADDRESS, call_validator::slot_validator_count())
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Write default governance config values to storage if not yet initialized.
    fn ensure_config_initialized(&mut self) {
        let initialized = self
            .backend
            .load(GOVERNANCE_ADDRESS, storage_slot(&[b"gov_config_initialized"]));
        if initialized != U256::ZERO {
            return;
        }
        use crate::config::GovernanceConfig;
        let cfg = GovernanceConfig::default();
        self.write_config_u32(b"validator_quorum_bps", cfg.validator_quorum_bps);
        self.write_config_u32(b"supply_quorum_bps", cfg.supply_quorum_bps);
        self.write_config_u32(b"treasury_quorum_bps", cfg.treasury_quorum_bps);
        self.write_config_u32(b"simple_majority_bps", cfg.simple_majority_bps);
        self.write_config_u32(b"emergency_pause_bps", cfg.emergency_pause_bps);
        self.backend.store(
            GOVERNANCE_ADDRESS,
            storage_slot(&[b"gov_config_initialized"]),
            U256::from(1u8),
        );
    }

    /// Load governance config from storage. Ensures defaults are written on first
    /// access so callers never read uninitialized zeros.
    fn governance_config(&mut self) -> crate::config::GovernanceConfig {
        self.ensure_config_initialized();
        crate::config::GovernanceConfig {
            validator_quorum_bps: self.read_config_u32(b"validator_quorum_bps"),
            supply_quorum_bps: self.read_config_u32(b"supply_quorum_bps"),
            treasury_quorum_bps: self.read_config_u32(b"treasury_quorum_bps"),
            simple_majority_bps: self.read_config_u32(b"simple_majority_bps"),
            emergency_pause_bps: self.read_config_u32(b"emergency_pause_bps"),
            review_period_blocks: self.read_config_u64(b"review_period"),
            voting_period_blocks: self.read_config_u64(b"voting_period"),
            timelock_period_blocks: self.read_config_u64(b"timelock"),
            execution_timeout_blocks: self.read_config_u64(b"execution_timeout"),
            proposal_deposit: self.read_config_u128(b"proposal_deposit"),
            asset_registration_fee: self.read_config_u128(b"asset_registration_fee"),
        }
    }

    /// Compute and store quorum_required for a proposal based on its type.
    fn compute_and_set_quorum(&mut self, proposal_id: u64) {
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");
        let config = self.governance_config();
        let validator_count = self.read_validator_count();

        let quorum = match proposal_type {
            2 => config.treasury_quorum(), // TreasurySpend: balance-weighted
            4 => config.supply_quorum(), // ComplianceUpdate: joint issuer+validator voting
            5 => {
                if validator_count == 0 {
                    0 // fallback when no validators registered
                } else {
                    config.emergency_pause_threshold(validator_count) as u128
                }
            }
            _ => {
                if validator_count == 0 {
                    0 // fallback when no validators registered
                } else {
                    config.validator_quorum(validator_count) as u128
                }
            }
        };

        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_quorum_required(proposal_id),
            u128_to_u256(quorum),
        );
    }

    // ── Config read helpers ─────────────────────────────────────────────

    pub fn read_config_u64(&mut self, suffix: &[u8]) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_config(suffix)),
        )
    }

    pub fn read_config_u128(&mut self, suffix: &[u8]) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_config(suffix)),
        )
    }

    pub fn read_config_u32(&mut self, suffix: &[u8]) -> u32 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_config(suffix)),
        ) as u32
    }

    pub fn write_config_u64(&mut self, suffix: &[u8], value: u64) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_config(suffix),
            u64_to_u256(value),
        );
    }

    pub fn write_config_u128(&mut self, suffix: &[u8], value: u128) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_config(suffix),
            u128_to_u256(value),
        );
    }

    pub fn write_config_u32(&mut self, suffix: &[u8], value: u32) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_config(suffix),
            u64_to_u256(value as u64),
        );
    }

    pub fn increment_tally(&mut self, proposal_id: u64, suffix: &[u8]) {
        let tally = self.read_vote_tally(proposal_id, suffix);
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, suffix),
            u128_to_u256(tally + 1),
        );
    }

    pub fn is_paused(&mut self) -> bool {
        self.backend
            .load(GOVERNANCE_ADDRESS, slot_gov_paused())
            .to_be_bytes::<32>()[31]
            != 0
    }

    /// Mark a proposal as Defeated and confiscate its deposit.
    fn mark_defeated(&mut self, proposal_id: u64) {
        let proposer = u256_to_address(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
        ));
        if proposer != Address::ZERO {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"deposit"),
                U256::ZERO,
            );
        }
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(4u8),
        );
    }

    pub fn submit_proposal(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        proposal_type: u8,
        title: String,
        description: String,
        execution_data: Vec<u8>,
        proposer: Address,
        current_block: u64,
    ) -> Result<u64, PrecompileError> {
        self.ensure_config_initialized();

        // Reject unsupported proposal types early
        match proposal_type {
            0..=10 => {}
            _ => {
                return Err(PrecompileError::Other(
                    "governance: unsupported proposal type".into(),
                ))
            }
        }

        // Rate limiting
        let cooldown = self.read_config_u64(b"proposal_cooldown");
        let cooldown = if cooldown == 0 {
            GOV_PROPOSAL_COOLDOWN
        } else {
            cooldown
        };
        let last_submit = u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_last_submission(proposer)),
        );
        if last_submit > 0 && current_block.saturating_sub(last_submit) < cooldown {
            return Err(PrecompileError::Other(
                "governance: proposal rate limited".into(),
            ));
        }

        // Validate proposal-specific execution data
        match proposal_type {
            3 => {
                // ValidatorSlash: require amount > 0
                if execution_data.len() < 64 {
                    return Err(PrecompileError::Other(
                        "governance: ValidatorSlash execution data too short"
                            .into(),
                    ));
                }
                let mut amount_buf = [0u8; 16];
                amount_buf.copy_from_slice(&execution_data[48..64]);
                let amount = u128::from_be_bytes(amount_buf);
                if amount == 0 {
                    return Err(PrecompileError::Other(
                        "governance: ValidatorSlash amount must be non-zero"
                            .into(),
                    ));
                }
            }
            _ => {}
        }

        asset_store
            .deduct_balance(CALL_ASSET_ID, proposer, PROPOSAL_DEPOSIT)
            .map_err(|_| {
                PrecompileError::Other(
                    "governance: insufficient balance for proposal deposit".into(),
                )
            })?;

        let count = self.read_proposal_count();
        let proposal_id = count + 1;

        let review_period = self.read_config_u64(b"review_period");
        let voting_period = self.read_config_u64(b"voting_period");

        let start_block = current_block
            .checked_add(review_period)
            .ok_or_else(|| PrecompileError::Other("governance: start_block overflow".into()))?;
        let end_block = start_block
            .checked_add(voting_period)
            .ok_or_else(|| PrecompileError::Other("governance: end_block overflow".into()))?;

        // If no review period, proposal is active immediately
        let initial_status: u8 = if review_period == 0 { 1 } else { 0 };

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
        // Store keccak256 hashes of title/description to fit in 32-byte slots
        let title_hash = alloy_primitives::keccak256(title.as_bytes());
        let desc_hash = alloy_primitives::keccak256(description.as_bytes());
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"title"),
            U256::from_be_slice(title_hash.as_slice()),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"desc"),
            U256::from_be_slice(desc_hash.as_slice()),
        );
        // Store execution_data hash for reference
        let data_hash = alloy_primitives::keccak256(&execution_data);
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"data"),
            U256::from_be_slice(data_hash.as_slice()),
        );
        // Store execution_data length and chunks for execute() to read back
        let data_len = execution_data.len() as u64;
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_data_len(proposal_id),
            u64_to_u256(data_len),
        );
        for (chunk_idx, chunk) in execution_data.chunks(32).enumerate() {
            let mut buf = [0u8; 32];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal_data_chunk(proposal_id, chunk_idx as u64),
                U256::from_be_slice(&buf),
            );
        }
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(initial_status),
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
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"start_block"),
            u64_to_u256(start_block),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"end_block"),
            u64_to_u256(end_block),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposal_type"),
            U256::from(proposal_type),
        );
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_review_end(proposal_id),
            u64_to_u256(start_block),
        );
        // Compute quorum AFTER proposal_type is stored so compute_and_set_quorum
        // reads the correct type.
        if review_period == 0 {
            self.compute_and_set_quorum(proposal_id);
        }
        // Quorum is computed and set by the advancer when transitioning to Active.
        // If there is a review period, zero it out here — the advancer (vote/queue)
        // will fill it in when auto-advancing Pending -> Active.
        if review_period > 0 {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal_quorum_required(proposal_id),
                U256::ZERO,
            );
        }

        // Update rate limit tracking
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_last_submission(proposer),
            u64_to_u256(current_block),
        );

        Ok(proposal_id)
    }

    pub fn vote(
        &mut self,
        proposal_id: u64,
        vote_val: u8,
        voter: Address,
        voting_power: u128,
        current_block: u64,
    ) -> Result<(), PrecompileError> {
        if vote_val < 1 || vote_val > 3 {
            return Err(PrecompileError::Other(
                "governance: invalid vote value".into(),
            ));
        }

        // Block all voting while chain is paused
        if self.is_paused() {
            return Err(PrecompileError::Other(
                "governance: chain is paused".into(),
            ));
        }

        self.require_valid_proposal_id(proposal_id)?;

        let status = self.read_proposal_status(proposal_id);
        let start_block = self.read_proposal_u64(proposal_id, b"start_block");
        let end_block = self.read_proposal_u64(proposal_id, b"end_block");

        // Auto-advance Pending -> Active if review period passed
        if status == 0 && current_block >= start_block {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"status"),
                U256::from(1u8),
            );
            self.compute_and_set_quorum(proposal_id);
        }

        let status = self.read_proposal_status(proposal_id);
        if status != 1 {
            return Err(PrecompileError::Other(
                "governance: proposal not active".into(),
            ));
        }

        if current_block < start_block {
            return Err(PrecompileError::Other(
                "governance: voting not started".into(),
            ));
        }
        if current_block >= end_block {
            return Err(PrecompileError::Other(
                "governance: voting period closed".into(),
            ));
        }

        let voter_slot = slot_gov_voter(proposal_id, voter);
        let has_voted = self.backend.load(GOVERNANCE_ADDRESS, voter_slot);
        if has_voted != U256::ZERO {
            return Err(PrecompileError::Other("governance: already voted".into()));
        }

        if voting_power == 0 {
            return Err(PrecompileError::Other("governance: no voting power".into()));
        }

        // Snapshot voting power at time of vote (prevents transfer-then-revote sybil)
        self.backend
            .store(GOVERNANCE_ADDRESS, voter_slot, u128_to_u256(voting_power));
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_voter_vote(proposal_id, voter),
            U256::from(vote_val),
        );

        let tally_suffix: &[u8] = match vote_val {
            1 => b"votes_for",
            2 => b"votes_against",
            3 => b"votes_abstain",
            _ => unreachable!(),
        };
        let tally = self.read_vote_tally(proposal_id, tally_suffix);
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, tally_suffix),
            u128_to_u256(tally.saturating_add(voting_power)),
        );

        Ok(())
    }

    pub fn queue(&mut self, proposal_id: u64, current_block: u64) -> Result<(), PrecompileError> {
        // Block queueing while chain is paused
        if self.is_paused() {
            return Err(PrecompileError::Other(
                "governance: chain is paused".into(),
            ));
        }

        self.require_valid_proposal_id(proposal_id)?;

        // Auto-advance Pending -> Active if review period passed
        let status = self.read_proposal_status(proposal_id);
        let start_block = self.read_proposal_u64(proposal_id, b"start_block");
        if status == 0 && current_block >= start_block {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"status"),
                U256::from(1u8),
            );
            self.compute_and_set_quorum(proposal_id);
        }

        self.require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        let votes_for = self.read_vote_tally(proposal_id, b"votes_for");
        let votes_against = self.read_vote_tally(proposal_id, b"votes_against");
        let votes_abstain = self.read_vote_tally(proposal_id, b"votes_abstain");
        let total_votes = votes_for
            .saturating_add(votes_against)
            .saturating_add(votes_abstain);

        // Per-type quorum check
        let quorum_required = self.read_proposal_u128(proposal_id, b"quorum_required");
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");

        // If quorum wasn't set by advancer, fall back to a safe default based on
        // validator count so a proposal never queues with an accidentally-zero threshold.
        let has_quorum = if quorum_required > 0 {
            total_votes >= quorum_required
        } else {
            let validator_count = self.read_validator_count();
            let fallback_threshold = if validator_count > 0 {
                validator_count as u128
            } else {
                1
            };
            total_votes >= fallback_threshold && votes_for > votes_against
        };

        if !has_quorum {
            self.mark_defeated(proposal_id);
            return Err(PrecompileError::Other(
                "governance: quorum not reached".into(),
            ));
        }

        // Simple majority check (configurable)
        let simple_majority_bps = self.read_config_u32(b"simple_majority_bps");
        let simple_majority_bps = if simple_majority_bps == 0 {
            crate::config::GovernanceConfig::default().simple_majority_bps
        } else {
            simple_majority_bps
        };
        let participation = votes_for.saturating_add(votes_against);
        let has_majority = if participation == 0 {
            false
        } else {
            // votes_for / participation > simple_majority_bps / 10000
            // => votes_for * 10000 > participation * simple_majority_bps
            let lhs = votes_for.saturating_mul(10_000);
            let rhs = participation.saturating_mul(simple_majority_bps as u128);
            lhs > rhs
        };
        if !has_majority {
            self.mark_defeated(proposal_id);
            return Err(PrecompileError::Other(
                "governance: not enough for votes".into(),
            ));
        }

        let timelock = self.read_config_u64(b"timelock");
        let timelock = if timelock == 0 {
            GOV_TIMELOCK_BLOCKS
        } else {
            timelock
        };

        // EmergencyPause skips timelock
        let exec_block = if proposal_type == 5 {
            current_block
        } else {
            current_block.checked_add(timelock).ok_or_else(|| {
                PrecompileError::Other("governance: timelock block overflow".into())
            })?
        };

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
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"execution_block"),
            u64_to_u256(exec_block),
        );

        Ok(())
    }

    pub fn execute(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        proposal_id: u64,
        current_block: u64,
        executor: Address,
    ) -> Result<(), PrecompileError> {
        self.require_valid_proposal_id(proposal_id)?;

        // Block execution while chain is paused (except EmergencyPause itself)
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");
        if self.is_paused() && proposal_type != 5 {
            return Err(PrecompileError::Other(
                "governance: chain is paused".into(),
            ));
        }

        self.require_proposal_status(proposal_id, 2, "governance: proposal not queued")?;

        let _queued_at = u256_to_u64(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"queued_at"),
        ));
        let execution_block = u256_to_u64(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"execution_block"),
        ));
        if current_block < execution_block {
            return Err(PrecompileError::Other(
                "governance: timelock not elapsed".into(),
            ));
        }

        // Execution timeout check
        let execution_timeout = self.read_config_u64(b"execution_timeout");
        let execution_timeout = if execution_timeout == 0 {
            GOV_EXEC_TIMEOUT
        } else {
            execution_timeout
        };
        let proposer = u256_to_address(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
        ));

        if current_block > execution_block.saturating_add(execution_timeout) {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"status"),
                U256::from(6u8), // Expired
            );
            // Refund deposit on expiration so tokens don't get stuck
            let deposit = u256_to_u128(self.backend.load(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"deposit"),
            ));
            if deposit > 0 && proposer != Address::ZERO {
                asset_store
                    .add_balance(CALL_ASSET_ID, proposer, deposit)
                    .map_err(|_| {
                        PrecompileError::Other(
                            "governance: deposit refund failed".into(),
                        )
                    })?;
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    slot_gov_proposal(proposal_id, b"deposit"),
                    U256::ZERO,
                );
            }
            return Err(PrecompileError::Other(
                "governance: execution timeout reached".into(),
            ));
        }

        let _ = executor;

        // Apply side effects based on proposal type
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");
        let execution_data = self.read_proposal_execution_data(proposal_id);
        let exec_result = match proposal_type {
            0 => {
                // ParameterChange: execution_data = ABI-encoded (string configKey, uint128 newValue)
                if execution_data.len() < 128 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ParameterChange execution data".into(),
                    ));
                }
                let string_offset =
                    u256_to_u64(U256::from_be_slice(&execution_data[0..32])) as usize;
                let string_end = string_offset.saturating_add(32);
                if execution_data.len() < string_end {
                    return Err(PrecompileError::Other(
                        "governance: malformed ParameterChange execution data".into(),
                    ));
                }
                let string_len = u256_to_u64(
                    U256::from_be_slice(&execution_data[string_offset..string_end]),
                ) as usize;
                let data_end = string_offset.saturating_add(32).saturating_add(string_len);
                if execution_data.len() < data_end {
                    return Err(PrecompileError::Other(
                        "governance: malformed ParameterChange execution data".into(),
                    ));
                }
                let key_bytes =
                    &execution_data[string_offset + 32..string_offset + 32 + string_len];
                let key = std::str::from_utf8(key_bytes).map_err(|_| {
                    PrecompileError::Other(
                        "governance: invalid UTF-8 in parameter key".into(),
                    )
                })?;
                if !ALLOWED_GOVERNANCE_PARAMS.contains(&key) {
                    return Err(PrecompileError::Other(
                        format!("governance: disallowed parameter key: {}", key).into(),
                    ));
                }
                let mut value_buf = [0u8; 16];
                value_buf.copy_from_slice(&execution_data[48..64]);
                let new_value = u128::from_be_bytes(value_buf);
                if let Some(err) = crate::config::validate_config_param(key, new_value) {
                    return Err(PrecompileError::Other(err.into()));
                }
                self.write_config_u128(key.as_bytes(), new_value);
                Ok(())
            }
            2 => {
                // TreasurySpend: execution_data = ABI-encoded (address recipient, uint128 amount, uint64 assetId)
                // Minimum 64 bytes for address + amount; asset_id is optional and defaults to CALL_ASSET_ID.
                if execution_data.len() < 64 {
                    return Err(PrecompileError::Other(
                        "governance: malformed TreasurySpend execution data".into(),
                    ));
                }
                let mut addr_buf = [0u8; 20];
                addr_buf.copy_from_slice(&execution_data[12..32]);
                let recipient = Address::from_slice(&addr_buf);
                if recipient == Address::ZERO {
                    return Err(PrecompileError::Other(
                        "governance: treasury spend recipient cannot be zero address"
                            .into(),
                    ));
                }
                let mut amount_buf = [0u8; 16];
                amount_buf.copy_from_slice(&execution_data[48..64]);
                let amount = u128::from_be_bytes(amount_buf);
                if amount == 0 {
                    return Err(PrecompileError::Other(
                        "governance: treasury spend amount must be non-zero"
                            .into(),
                    ));
                }
                let asset_id = if execution_data.len() >= 96 {
                    let mut asset_buf = [0u8; 8];
                    asset_buf.copy_from_slice(&execution_data[88..96]);
                    u64::from_be_bytes(asset_buf)
                } else {
                    CALL_ASSET_ID
                };
                asset_store
                    .deduct_balance(asset_id, TREASURY_ADDRESS, amount)
                    .map_err(|_| {
                        PrecompileError::Other(
                            "governance: treasury insufficient balance".into(),
                        )
                    })?;
                asset_store
                    .add_balance(asset_id, recipient, amount)
                    .map_err(|_| {
                        PrecompileError::Other(
                            "governance: treasury spend credit failed".into(),
                        )
                    })?;
                Ok(())
            }
            5 => {
                // EmergencyPause: set paused flag
                self.backend
                    .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
                Ok(())
            }
            6 => {
                // FeeCurrencyAdd: execution_data = ABI-encoded (uint64 assetId)
                if execution_data.len() < 32 {
                    return Err(PrecompileError::Other(
                        "governance: malformed FeeCurrencyAdd execution data".into(),
                    ));
                }
                let mut asset_buf = [0u8; 8];
                asset_buf.copy_from_slice(&execution_data[24..32]);
                let asset_id = u64::from_be_bytes(asset_buf);
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"fee_currency", &asset_id.to_be_bytes()[..]]),
                    U256::from(1u8),
                );
                Ok(())
            }
            7 => {
                // FeeCurrencyRemove: execution_data = ABI-encoded (uint64 assetId)
                if execution_data.len() < 32 {
                    return Err(PrecompileError::Other(
                        "governance: malformed FeeCurrencyRemove execution data".into(),
                    ));
                }
                let mut asset_buf = [0u8; 8];
                asset_buf.copy_from_slice(&execution_data[24..32]);
                let asset_id = u64::from_be_bytes(asset_buf);
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"fee_currency", &asset_id.to_be_bytes()[..]]),
                    U256::ZERO,
                );
                Ok(())
            }
            9 => {
                // ValidatorKeyRotation: execution_data = ABI-encoded
                // (uint64 validatorId, bytes32 oldPubkey, bytes32 newPubkey)
                if execution_data.len() < 96 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ValidatorKeyRotation execution data".into(),
                    ));
                }
                let mut id_buf = [0u8; 8];
                id_buf.copy_from_slice(&execution_data[24..32]);
                let _validator_id = u64::from_be_bytes(id_buf);

                let mut old_pubkey = [0u8; 32];
                old_pubkey.copy_from_slice(&execution_data[32..64]);
                if old_pubkey == [0u8; 32] {
                    return Err(PrecompileError::Other(
                        "governance: old pubkey must be non-zero".into(),
                    ));
                }

                let mut new_pubkey = [0u8; 32];
                new_pubkey.copy_from_slice(&execution_data[64..96]);
                if new_pubkey == [0u8; 32] {
                    return Err(PrecompileError::Other(
                        "governance: new pubkey must be non-zero".into(),
                    ));
                }

                // TODO: ed25519 signature verification pending (#48)

                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"key_rotation", &id_buf, b"old"]),
                    U256::from_be_slice(&old_pubkey),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"key_rotation", &id_buf, b"new"]),
                    U256::from_be_slice(&new_pubkey),
                );
                Ok(())
            }
            1 => {
                // ProtocolUpgrade: execution_data = ABI-encoded (bytes32 newVersionHash)
                if execution_data.len() < 32 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ProtocolUpgrade execution data".into(),
                    ));
                }
                let mut version_buf = [0u8; 32];
                version_buf.copy_from_slice(&execution_data[0..32]);
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"protocol_upgrade"]),
                    U256::from_be_slice(&version_buf),
                );
                Ok(())
            }
            3 => {
                // ValidatorSlash: execution_data = ABI-encoded (address validator, uint128 amount)
                if execution_data.len() < 64 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ValidatorSlash execution data".into(),
                    ));
                }
                let mut addr_buf = [0u8; 20];
                addr_buf.copy_from_slice(&execution_data[12..32]);
                let validator = Address::from_slice(&addr_buf);
                let mut amount_buf = [0u8; 16];
                amount_buf.copy_from_slice(&execution_data[48..64]);
                let amount = u128::from_be_bytes(amount_buf);
                let stake_slot = storage_slot(&[validator.as_slice(), b"stake"]);
                let current_stake =
                    u256_to_u128(self.backend.load(VALIDATOR_ADDRESS, stake_slot));
                let new_stake = current_stake.saturating_sub(amount);
                self.backend
                    .store(VALIDATOR_ADDRESS, stake_slot, u128_to_u256(new_stake));
                if new_stake == 0 {
                    let status_slot = storage_slot(&[validator.as_slice(), b"status"]);
                    self.backend
                        .store(VALIDATOR_ADDRESS, status_slot, U256::ZERO);
                }
                Ok(())
            }
            4 => {
                // ComplianceUpdate: execution_data = ABI-encoded (address target, uint8 status)
                if execution_data.len() < 64 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ComplianceUpdate execution data".into(),
                    ));
                }
                let mut addr_buf = [0u8; 20];
                addr_buf.copy_from_slice(&execution_data[12..32]);
                let target = Address::from_slice(&addr_buf);
                let status = execution_data[63];
                if status > 1 {
                    return Err(PrecompileError::Other(
                        "governance: invalid compliance status (must be 0 or 1)".into(),
                    ));
                }
                self.backend.store(
                    COMPLIANCE_ADDRESS,
                    slot_compliance(target),
                    U256::from(status),
                );
                Ok(())
            }
            8 => {
                // FeeCurrencyCap: execution_data = ABI-encoded (uint64 assetId, uint128 cap)
                if execution_data.len() < 64 {
                    return Err(PrecompileError::Other(
                        "governance: malformed FeeCurrencyCap execution data".into(),
                    ));
                }
                let mut asset_buf = [0u8; 8];
                asset_buf.copy_from_slice(&execution_data[24..32]);
                let asset_id = u64::from_be_bytes(asset_buf);
                let mut cap_buf = [0u8; 16];
                cap_buf.copy_from_slice(&execution_data[48..64]);
                let cap = u128::from_be_bytes(cap_buf);
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"fee_currency_cap", &asset_id.to_be_bytes()[..]]),
                    u128_to_u256(cap),
                );
                Ok(())
            }
            10 => {
                // ProverKeyRotation: execution_data = ABI-encoded
                // (uint32 keyVersion, bytes32 transferVkHash, bytes32 depositVkHash, bytes32 withdrawVkHash, uint64 sunsetTimestamp)
                if execution_data.len() < 160 {
                    return Err(PrecompileError::Other(
                        "governance: malformed ProverKeyRotation execution data".into(),
                    ));
                }
                let mut version_buf = [0u8; 4];
                version_buf.copy_from_slice(&execution_data[28..32]);
                let key_version = u32::from_be_bytes(version_buf);

                let mut transfer_hash = [0u8; 32];
                transfer_hash.copy_from_slice(&execution_data[32..64]);
                let mut deposit_hash = [0u8; 32];
                deposit_hash.copy_from_slice(&execution_data[64..96]);
                let mut withdraw_hash = [0u8; 32];
                withdraw_hash.copy_from_slice(&execution_data[96..128]);
                let mut sunset_buf = [0u8; 8];
                sunset_buf.copy_from_slice(&execution_data[152..160]);
                let sunset_timestamp = u64::from_be_bytes(sunset_buf);
                if sunset_timestamp == 0 {
                    return Err(PrecompileError::Other(
                        "governance: sunset timestamp must be non-zero".into(),
                    ));
                }

                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"version"]),
                    U256::from(key_version),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"transfer_vk_hash"]),
                    U256::from_be_slice(&transfer_hash),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"deposit_vk_hash"]),
                    U256::from_be_slice(&deposit_hash),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"withdraw_vk_hash"]),
                    U256::from_be_slice(&withdraw_hash),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"sunset_timestamp"]),
                    U256::from(sunset_timestamp),
                );
                self.backend.store(
                    GOVERNANCE_ADDRESS,
                    storage_slot(&[b"prover_key_rotation", b"pending"]),
                    U256::from(1u8),
                );
                Ok(())
            }
            _ => Err(PrecompileError::Other(
                format!("governance: unsupported proposal type {}", proposal_type).into(),
            )),
        };
        if let Err(e) = exec_result {
            return Err(e);
        }

        let deposit = u256_to_u128(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"deposit"),
        ));
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

    // ── Emergency multi-sig helpers ───────────────────────────────────

    pub fn read_emergency_pause_epoch(&mut self) -> u64 {
        u256_to_u64(self.backend.load(GOVERNANCE_ADDRESS, slot_emergency_pause_epoch()))
    }

    pub fn write_emergency_pause_epoch(&mut self, epoch: u64) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_pause_epoch(),
            u64_to_u256(epoch),
        );
    }

    pub fn increment_emergency_pause_epoch(&mut self) {
        let epoch = self.read_emergency_pause_epoch();
        self.write_emergency_pause_epoch(epoch + 1);
    }

    pub fn read_emergency_pause_sig_epoch(&mut self, addr: Address) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_emergency_pause_sig_epoch(addr)),
        )
    }

    pub fn write_emergency_pause_sig_epoch(&mut self, addr: Address, epoch: u64) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_pause_sig_epoch(addr),
            u64_to_u256(epoch),
        );
    }

    pub fn read_emergency_pause_sig_count(&mut self) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_emergency_pause_sig_count()),
        )
    }

    pub fn increment_emergency_pause_sig_count(&mut self) -> u64 {
        let count = self.read_emergency_pause_sig_count();
        let new_count = count + 1;
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_pause_sig_count(),
            u64_to_u256(new_count),
        );
        new_count
    }

    pub fn reset_emergency_pause_sig_count(&mut self) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_pause_sig_count(),
            U256::ZERO,
        );
    }

    pub fn read_emergency_resume_epoch(&mut self) -> u64 {
        u256_to_u64(self.backend.load(GOVERNANCE_ADDRESS, slot_emergency_resume_epoch()))
    }

    pub fn write_emergency_resume_epoch(&mut self, epoch: u64) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_resume_epoch(),
            u64_to_u256(epoch),
        );
    }

    pub fn increment_emergency_resume_epoch(&mut self) {
        let epoch = self.read_emergency_resume_epoch();
        self.write_emergency_resume_epoch(epoch + 1);
    }

    pub fn read_emergency_resume_sig_epoch(&mut self, addr: Address) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_emergency_resume_sig_epoch(addr)),
        )
    }

    pub fn write_emergency_resume_sig_epoch(&mut self, addr: Address, epoch: u64) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_resume_sig_epoch(addr),
            u64_to_u256(epoch),
        );
    }

    pub fn read_emergency_resume_sig_count(&mut self) -> u64 {
        u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_emergency_resume_sig_count()),
        )
    }

    pub fn increment_emergency_resume_sig_count(&mut self) -> u64 {
        let count = self.read_emergency_resume_sig_count();
        let new_count = count + 1;
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_resume_sig_count(),
            u64_to_u256(new_count),
        );
        new_count
    }

    pub fn reset_emergency_resume_sig_count(&mut self) {
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_emergency_resume_sig_count(),
            U256::ZERO,
        );
    }

    pub fn emergency_pause(
        &mut self,
        reason: [u8; 32],
        _pauser: Address,
    ) -> Result<(), PrecompileError> {
        if self.is_paused() {
            return Err(PrecompileError::Other(
                "governance: already paused".into(),
            ));
        }
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_pause_reason(),
            U256::from_be_slice(&reason),
        );
        Ok(())
    }

    pub fn emergency_resume(&mut self) -> Result<(), PrecompileError> {
        if !self.is_paused() {
            return Err(PrecompileError::Other(
                "governance: not paused".into(),
            ));
        }
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::ZERO);
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_pause_reason(), U256::ZERO);
        Ok(())
    }

    pub fn cancel_proposal(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        proposal_id: u64,
        caller: Address,
    ) -> Result<(), PrecompileError> {
        if self.is_paused() {
            return Err(PrecompileError::Other(
                "governance: chain is paused".into(),
            ));
        }

        self.require_valid_proposal_id(proposal_id)?;

        let proposer = self.read_proposal_proposer(proposal_id);
        if caller != proposer {
            return Err(PrecompileError::Other(
                "governance: only proposer can cancel".into(),
            ));
        }

        let status = self.read_proposal_status(proposal_id);
        if status != 0 {
            return Err(PrecompileError::Other(
                "governance: can only cancel during review period".into(),
            ));
        }

        // Refund deposit
        let deposit = self.read_proposal_u128(proposal_id, b"deposit");
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

        // Mark as Defeated
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"status"),
            U256::from(4u8),
        );

        Ok(())
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolGovernance {
        function submitProposal(uint8 proposalType, string title, string description, bytes executionData) external;
        function vote(uint64 proposalId, uint8 vote) external;
        function queue(uint64 proposalId) external;
        function execute(uint64 proposalId) external;
        function cancelProposal(uint64 proposalId) external;
        function emergencyPause(bytes32 reason) external;
        function emergencyResume() external;
        function getProposalStatus(uint64 proposalId) external view returns (uint8);
        function getProposalVotes(uint64 proposalId) external view returns (uint128 votesFor, uint128 votesAgainst, uint128 votesAbstain);
        function getProposal(uint64 proposalId) external view returns (uint8 status, uint8 proposalType, address proposer, uint64 startBlock, uint64 endBlock, uint128 votesFor, uint128 votesAgainst, uint128 votesAbstain, uint128 quorumRequired);
        function isPaused() external view returns (uint8);
        function getProposalCount() external view returns (uint64);
    }
}

// ── GovernancePrecompile ──────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct GovernancePrecompile;

impl GovernancePrecompile {
    fn submit_proposal(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        // Dynamic gas: base 100k + 5k per 32-byte chunk of execution data
        let base_gas = 100_000u64;
        let chunk_gas = 5_000u64;
        let max_gas = 1_000_000u64;
        let data_len = calldata.len().saturating_sub(4) as u64; // approximate, actual decoded data may differ
        let num_chunks = (data_len + 31) / 32;
        let gas = (base_gas + num_chunks * chunk_gas).min(max_gas);

        dispatch::mutate_void::<IProtocolGovernance::submitProposalCall, _>(
            calldata,
            gas,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                let mut gov_store = GovernanceStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_number = storage.block_number();
                let proposal_id = gov_store
                    .submit_proposal(
                        &mut asset_store,
                        call.proposalType,
                        call.title.to_string(),
                        call.description.to_string(),
                        call.executionData.to_vec(),
                        caller,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Emit ProposalSubmitted(uint64 indexed proposalId, address indexed proposer)
                let topic0 = alloy_primitives::keccak256(b"ProposalSubmitted(uint64,address)");
                let topic1 = alloy_primitives::B256::from(u64_to_u256(proposal_id).to_be_bytes::<32>());
                let topic2 = alloy_primitives::B256::from(address_to_u256(caller).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::new(),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn vote(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::voteCall, _>(
            calldata,
            10_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut gov_store = GovernanceStorage::new(sr);

                // Compute voting power based on proposal type
                let proposal_type = gov_store.read_proposal_u8(call.proposalId, b"proposal_type");
                let mut voting_power: u128 = 0;

                // Validator check (1=1 for validator proposals, joint voting)
                let validator_id = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller)
                };
                let is_validator = validator_id != 0;

                // CALL balance check
                let call_balance = {
                    let mut asset_store = AssetStorage::new(sr);
                    asset_store.read_balance(CALL_ASSET_ID, caller)
                };

                match proposal_type {
                    0 | 1 | 3 => {
                        // ParameterChange, ProtocolUpgrade, ValidatorSlash: validator 1=1 + balance weighted
                        if is_validator {
                            voting_power = voting_power.max(1);
                        }
                        voting_power = voting_power.max(call_balance);
                    }
                    2 => {
                        // TreasurySpend: CALL balance weighted
                        voting_power = call_balance;
                    }
                    4 => {
                        // ComplianceUpdate: joint issuer+validator voting (additive)
                        if is_validator {
                            voting_power = voting_power.saturating_add(1);
                        }
                        voting_power = voting_power.saturating_add(call_balance);
                    }
                    5 => {
                        // EmergencyPause: validator only, 1=1
                        if is_validator {
                            voting_power = 1;
                        }
                    }
                    6 | 7 | 8 | 9 | 10 => {
                        // FeeCurrencyAdd, FeeCurrencyRemove, FeeCurrencyCap, ValidatorKeyRotation, ProverKeyRotation: simple majority
                        if is_validator {
                            voting_power = voting_power.max(1);
                        }
                        voting_power = voting_power.max(call_balance);
                    }
                    _ => {
                        // Unknown type: default to balance weighted
                        voting_power = call_balance;
                    }
                }

                let block_number = storage.block_number();
                gov_store
                    .vote(
                        call.proposalId,
                        call.vote,
                        caller,
                        voting_power,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Emit VoteCast(uint64 indexed proposalId, address indexed voter, uint8 vote, uint128 power)
                let topic0 = alloy_primitives::keccak256(b"VoteCast(uint64,address,uint8,uint128)");
                let topic1 = alloy_primitives::B256::from(u64_to_u256(call.proposalId).to_be_bytes::<32>());
                let topic2 = alloy_primitives::B256::from(address_to_u256(caller).to_be_bytes::<32>());
                let mut event_data = Vec::with_capacity(64);
                event_data.extend_from_slice(&U256::from(call.vote).to_be_bytes::<32>());
                event_data.extend_from_slice(&u128_to_u256(voting_power).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn queue(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::queueCall, _>(
            calldata,
            20_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut gov_store = GovernanceStorage::new(sr);

                // Authorization: only proposer or validator may queue
                let proposer = gov_store.read_proposal_proposer(call.proposalId);
                let is_proposer = caller == proposer;
                let is_validator = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller) != 0
                };
                if !is_proposer && !is_validator {
                    return Err(PrecompileError::Other(
                        "governance: unauthorized to queue".into(),
                    ));
                }

                let block_number = storage.block_number();
                let queue_result = gov_store.queue(call.proposalId, block_number);

                // Terminal state: emit ProposalDefeated if queue marked proposal defeated
                if queue_result.is_err() {
                    let status = gov_store.read_proposal_status(call.proposalId);
                    if status == 4 {
                        let topic0 =
                            alloy_primitives::keccak256(b"ProposalDefeated(uint64)");
                        let topic1 = alloy_primitives::B256::from(u64_to_u256(call.proposalId).to_be_bytes::<32>());
                        if let Some(log) = alloy_primitives::LogData::new(
                            vec![topic0, topic1],
                            alloy_primitives::Bytes::new(),
                        ) {
                            let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                        }
                    }
                }

                queue_result.map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Emit ProposalQueued(uint64 indexed proposalId, uint64 indexed queuedAt)
                let topic0 = alloy_primitives::keccak256(b"ProposalQueued(uint64,uint64)");
                let topic1 = alloy_primitives::B256::from(u64_to_u256(call.proposalId).to_be_bytes::<32>());
                let topic2 = alloy_primitives::B256::from(u64_to_u256(block_number).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::new(),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn execute(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        // Approximate proposalId from ABI calldata for dynamic gas estimation.
        let proposal_id = if calldata.len() >= 36 {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&calldata[28..36]);
            u64::from_be_bytes(buf)
        } else {
            0
        };
        let mut gov_store = GovernanceStorage::new(sr);
        let data_len = gov_store.read_proposal_data_len(proposal_id);
        let base_gas = 20_000u64;
        let chunk_gas = 500u64; // per 32-byte chunk of execution_data
        let max_gas = 100_000u64;
        let num_chunks = (data_len + 31) / 32;
        let gas = (base_gas + num_chunks * chunk_gas).min(max_gas);

        dispatch::mutate_void::<IProtocolGovernance::executeCall, _>(
            calldata,
            gas,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut gov_store = GovernanceStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);

                let block_number = storage.block_number();
                let exec_result =
                    gov_store.execute(&mut asset_store, call.proposalId, block_number, caller);

                // Terminal state: emit ProposalExpired if execute marked proposal expired
                if exec_result.is_err() {
                    let status = gov_store.read_proposal_status(call.proposalId);
                    if status == 6 {
                        let topic0 =
                            alloy_primitives::keccak256(b"ProposalExpired(uint64)");
                        let topic1 = alloy_primitives::B256::from(u64_to_u256(call.proposalId).to_be_bytes::<32>());
                        if let Some(log) = alloy_primitives::LogData::new(
                            vec![topic0, topic1],
                            alloy_primitives::Bytes::new(),
                        ) {
                            let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                        }
                    }
                }

                exec_result.map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Emit ProposalExecuted(uint64 indexed proposalId)
                let topic0 = alloy_primitives::keccak256(b"ProposalExecuted(uint64)");
                let topic1 = alloy_primitives::B256::from(u64_to_u256(call.proposalId).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::new(),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                // Type-specific execution events
                let proposal_type = gov_store.read_proposal_u8(call.proposalId, b"proposal_type");
                let execution_data = gov_store.read_proposal_execution_data(call.proposalId);
                match proposal_type {
                    3 => {
                        // ValidatorSlashed(address indexed validator, uint128 amount)
                        if execution_data.len() >= 64 {
                            let mut addr_buf = [0u8; 20];
                            addr_buf.copy_from_slice(&execution_data[12..32]);
                            let validator = Address::from_slice(&addr_buf);
                            let mut amount_buf = [0u8; 16];
                            amount_buf.copy_from_slice(&execution_data[48..64]);
                            let amount = u128::from_be_bytes(amount_buf);
                            let topic0 =
                                alloy_primitives::keccak256(b"ValidatorSlashed(address,uint128)");
                            let topic1 = alloy_primitives::B256::from(address_to_u256(validator).to_be_bytes::<32>());
                            let mut data = Vec::with_capacity(32);
                            data.extend_from_slice(&u128_to_u256(amount).to_be_bytes::<32>());
                            if let Some(log) = alloy_primitives::LogData::new(
                                vec![topic0, topic1],
                                alloy_primitives::Bytes::from(data),
                            ) {
                                let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                            }
                        }
                    }
                    4 => {
                        // ComplianceUpdated(address indexed target, uint8 status)
                        if execution_data.len() >= 64 {
                            let mut addr_buf = [0u8; 20];
                            addr_buf.copy_from_slice(&execution_data[12..32]);
                            let target = Address::from_slice(&addr_buf);
                            let status = execution_data[63];
                            let topic0 =
                                alloy_primitives::keccak256(b"ComplianceUpdated(address,uint8)");
                            let topic1 = alloy_primitives::B256::from(address_to_u256(target).to_be_bytes::<32>());
                            let mut data = Vec::with_capacity(32);
                            data.extend_from_slice(&U256::from(status).to_be_bytes::<32>());
                            if let Some(log) = alloy_primitives::LogData::new(
                                vec![topic0, topic1],
                                alloy_primitives::Bytes::from(data),
                            ) {
                                let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                            }
                        }
                    }
                    6 | 7 | 8 => {
                        // FeeCurrencyAdded/Removed/Capped(uint64 indexed assetId)
                        if execution_data.len() >= 32 {
                            let mut asset_buf = [0u8; 8];
                            asset_buf.copy_from_slice(&execution_data[24..32]);
                            let asset_id = u64::from_be_bytes(asset_buf);
                            let topic0 = match proposal_type {
                                6 => alloy_primitives::keccak256(b"FeeCurrencyAdded(uint64)"),
                                7 => alloy_primitives::keccak256(b"FeeCurrencyRemoved(uint64)"),
                                8 => alloy_primitives::keccak256(b"FeeCurrencyCapped(uint64)"),
                                _ => unreachable!(),
                            };
                            let topic1 = alloy_primitives::B256::from(u64_to_u256(asset_id).to_be_bytes::<32>());
                            if let Some(log) = alloy_primitives::LogData::new(
                                vec![topic0, topic1],
                                alloy_primitives::Bytes::new(),
                            ) {
                                let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                            }
                        }
                    }
                    _ => {}
                }

                Ok(())
            },
        )
    }

    fn emergency_pause(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::emergencyPauseCall, _>(
            calldata,
            30_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                let mut gov_store = GovernanceStorage::new(sr);

                // Reject if already paused — must be checked before recording signatures
                // so the multi-sig state doesn't get stuck.
                if gov_store.is_paused() {
                    return Err(PrecompileError::Other(
                        "governance: already paused".into(),
                    ));
                }

                // Verify caller is a registered validator
                let validator_id = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller)
                };
                if validator_id == 0 {
                    return Err(PrecompileError::Other(
                        "sender not a registered validator".into(),
                    ));
                }

                // Multi-sig: epoch-based signature tracking
                let mut epoch = gov_store.read_emergency_pause_epoch();
                if epoch == 0 {
                    epoch = 1;
                    gov_store.write_emergency_pause_epoch(1);
                }
                let last_signed = gov_store.read_emergency_pause_sig_epoch(caller);
                if last_signed >= epoch {
                    return Err(PrecompileError::Other(
                        "governance: already signed emergency pause".into(),
                    ));
                }

                // Record signature
                gov_store.write_emergency_pause_sig_epoch(caller, epoch);
                let count = gov_store.increment_emergency_pause_sig_count();

                // Check threshold
                let validator_count = gov_store.read_validator_count();
                let config = gov_store.governance_config();
                let threshold = config.emergency_pause_threshold(validator_count);

                if count < threshold {
                    // Not yet enough signatures; return Ok but nothing happened yet
                    return Ok(());
                }

                // Threshold reached — execute pause
                gov_store
                    .emergency_pause(call.reason.into(), caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Reset for next round
                gov_store.increment_emergency_pause_epoch();
                gov_store.reset_emergency_pause_sig_count();

                // Emit EmergencyPaused(address indexed pauser, bytes32 indexed reason)
                let topic0 = alloy_primitives::keccak256(b"EmergencyPaused(address,bytes32)");
                let topic1 = alloy_primitives::B256::from(address_to_u256(caller).to_be_bytes::<32>());
                let topic2 = alloy_primitives::B256::from_slice(call.reason.as_ref());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::new(),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn emergency_resume(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::emergencyResumeCall, _>(
            calldata,
            20_000,
            storage,
            |_call, storage| {
                let caller = require_caller(msg_sender)?;

                let mut gov_store = GovernanceStorage::new(sr);

                // Reject if not paused — must be checked before recording signatures
                // so the multi-sig state doesn't get stuck.
                if !gov_store.is_paused() {
                    return Err(PrecompileError::Other(
                        "governance: not paused".into(),
                    ));
                }

                // Verify caller is a registered validator
                let validator_id = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller)
                };
                if validator_id == 0 {
                    return Err(PrecompileError::Other(
                        "sender not a registered validator".into(),
                    ));
                }

                // Multi-sig: epoch-based signature tracking
                let mut epoch = gov_store.read_emergency_resume_epoch();
                if epoch == 0 {
                    epoch = 1;
                    gov_store.write_emergency_resume_epoch(1);
                }
                let last_signed = gov_store.read_emergency_resume_sig_epoch(caller);
                if last_signed >= epoch {
                    return Err(PrecompileError::Other(
                        "governance: already signed emergency resume".into(),
                    ));
                }

                // Record signature
                gov_store.write_emergency_resume_sig_epoch(caller, epoch);
                let count = gov_store.increment_emergency_resume_sig_count();

                // Check threshold
                let validator_count = gov_store.read_validator_count();
                let config = gov_store.governance_config();
                let threshold = config.emergency_pause_threshold(validator_count);

                if count < threshold {
                    return Ok(());
                }

                // Threshold reached — execute resume
                gov_store
                    .emergency_resume()
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Reset for next round
                gov_store.increment_emergency_resume_epoch();
                gov_store.reset_emergency_resume_sig_count();

                // Emit EmergencyResumed(address indexed resumer)
                let topic0 = alloy_primitives::keccak256(b"EmergencyResumed(address)");
                let topic1 = alloy_primitives::B256::from(address_to_u256(caller).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::new(),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn cancel_proposal(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::cancelProposalCall, _>(
            calldata,
            20_000,
            storage,
            |_call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut gov_store = GovernanceStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                gov_store
                    .cancel_proposal(&mut asset_store, _call.proposalId, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn get_proposal(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalCall, _, _>(
            calldata,
            5000,
            storage,
            |call, _storage| {
                let mut store = GovernanceStorage::new(sr);
                store.require_valid_proposal_id(call.proposalId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                let status = store.read_proposal_status(call.proposalId);
                let proposal_type = store.read_proposal_u8(call.proposalId, b"proposal_type");
                let proposer = store.read_proposal_proposer(call.proposalId);
                let start_block = store.read_proposal_u64(call.proposalId, b"start_block");
                let end_block = store.read_proposal_u64(call.proposalId, b"end_block");
                let votes_for = store.read_vote_tally(call.proposalId, b"votes_for");
                let votes_against = store.read_vote_tally(call.proposalId, b"votes_against");
                let votes_abstain = store.read_vote_tally(call.proposalId, b"votes_abstain");
                let quorum_required = store.read_proposal_u128(call.proposalId, b"quorum_required");
                Ok((
                    U256::from(status),
                    U256::from(proposal_type),
                    proposer,
                    start_block,
                    end_block,
                    votes_for,
                    votes_against,
                    votes_abstain,
                    quorum_required,
                ))
            },
        )
    }

    fn get_proposal_status(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalStatusCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = GovernanceStorage::new(sr);
                store.require_valid_proposal_id(call.proposalId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(U256::from(store.read_proposal_status(call.proposalId)))
            },
        )
    }

    fn get_proposal_votes(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalVotesCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = GovernanceStorage::new(sr);
                store.require_valid_proposal_id(call.proposalId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                let votes_for = store.read_vote_tally(call.proposalId, b"votes_for");
                let votes_against = store.read_vote_tally(call.proposalId, b"votes_against");
                let votes_abstain = store.read_vote_tally(call.proposalId, b"votes_abstain");
                Ok((votes_for, votes_against, votes_abstain))
            },
        )
    }

    fn is_paused(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::isPausedCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = GovernanceStorage::new(sr);
                Ok(U256::from(if store.is_paused() { 1u8 } else { 0u8 }))
            },
        )
    }

    fn get_proposal_count(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolGovernance::getProposalCountCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = GovernanceStorage::new(sr);
                Ok(store.read_proposal_count())
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for GovernancePrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("invariant: 4-byte selector");
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolGovernance::submitProposalCall::SELECTOR => {
                self.submit_proposal(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::voteCall::SELECTOR => self.vote(calldata, msg_sender, storage, sr),
            IProtocolGovernance::queueCall::SELECTOR => {
                self.queue(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::executeCall::SELECTOR => {
                self.execute(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::cancelProposalCall::SELECTOR => {
                self.cancel_proposal(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::emergencyPauseCall::SELECTOR => {
                self.emergency_pause(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::emergencyResumeCall::SELECTOR => {
                self.emergency_resume(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::getProposalStatusCall::SELECTOR => {
                self.get_proposal_status(calldata, storage, sr)
            }
            IProtocolGovernance::getProposalVotesCall::SELECTOR => {
                self.get_proposal_votes(calldata, storage, sr)
            }
            IProtocolGovernance::getProposalCall::SELECTOR => {
                self.get_proposal(calldata, storage, sr)
            }
            IProtocolGovernance::isPausedCall::SELECTOR => self.is_paused(calldata, storage, sr),
            IProtocolGovernance::getProposalCountCall::SELECTOR => {
                self.get_proposal_count(calldata, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        slot_balance, slot_validator_by_addr, u128_to_u256, u64_to_u256, StatefulPrecompile,
    };
    use call_precompile::{ASSET_ADDRESS, VALIDATOR_ADDRESS};

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

        // Seed sender balance
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(PROPOSAL_DEPOSIT * 10),
            )
            .unwrap();
        // Seed review_period=0 so proposal is active immediately
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"review_period"),
                u64_to_u256(0),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // submitProposal (type 0 = ParameterChange)
        let input = IProtocolGovernance::submitProposalCall {
            proposalType: 0,
            title: "My Proposal".into(),
            description: "Description".into(),
            executionData: alloy_primitives::Bytes::from_static(b"param_data"),
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "submit failed: {:?}", result.err());

        // getProposalCount
        let input = IProtocolGovernance::getProposalCountCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let count = u256_to_u64(U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(count, 1);

        // getProposalStatus(1)
        let input = IProtocolGovernance::getProposalStatusCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1); // active

        // getProposalVotes(1)
        let input = IProtocolGovernance::getProposalVotesCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(&result.bytes[16..32], &[0u8; 16]); // votes_for = 0
        assert_eq!(&result.bytes[48..64], &[0u8; 16]); // votes_against = 0
        assert_eq!(&result.bytes[80..96], &[0u8; 16]); // votes_abstain = 0
    }

    #[test]
    fn test_governance_precompile_vote_queue_execute() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        // Seed sender balance
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(PROPOSAL_DEPOSIT * 10),
            )
            .unwrap();
        // Seed review_period=0 so proposal is active immediately
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"review_period"),
                u64_to_u256(0),
            )
            .unwrap();
        // Seed voting_period so votes have a window
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"voting_period"),
                u64_to_u256(100),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // submitProposal (type 0 = ParameterChange)
        let input = IProtocolGovernance::submitProposalCall {
            proposalType: 0,
            title: "Proposal".into(),
            description: "Desc".into(),
            executionData: alloy_primitives::Bytes::from_static(&[0xEEu8; 32]),
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // vote(proposalId=1, vote=1=Yes)
        // With type 0 (ParameterChange), voting power = max(1 if validator, call_balance)
        // Sender had PROPOSAL_DEPOSIT * 10 but PROPOSAL_DEPOSIT was deducted as deposit,
        // so voting power = PROPOSAL_DEPOSIT * 9
        let input = IProtocolGovernance::voteCall {
            proposalId: 1,
            vote: 1,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "vote failed: {:?}", result.err());

        // getProposalVotes(1)
        let input = IProtocolGovernance::getProposalVotesCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let votes_for = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&result.bytes[16..32]);
            buf
        });
        assert_eq!(votes_for, PROPOSAL_DEPOSIT * 9);

        // queue(1)
        let input = IProtocolGovernance::queueCall { proposalId: 1 }.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "queue failed: {:?}", result.err());

        // getProposalStatus(1) should be 2 (queued)
        let input = IProtocolGovernance::getProposalStatusCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 2);
    }

    #[test]
    fn test_governance_precompile_queue_unauthorized() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let proposer = Address::repeat_byte(0x44);
        let rando = Address::repeat_byte(0x55);

        // Seed proposer balance
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, proposer),
                u128_to_u256(PROPOSAL_DEPOSIT * 10),
            )
            .unwrap();
        // Seed review_period=0 so proposal is active immediately
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"review_period"),
                u64_to_u256(0),
            )
            .unwrap();
        // Seed voting_period so votes have a window
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"voting_period"),
                u64_to_u256(100),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // submitProposal
        let input = IProtocolGovernance::submitProposalCall {
            proposalType: 0,
            title: "Proposal".into(),
            description: "Desc".into(),
            executionData: alloy_primitives::Bytes::from_static(&[0xEEu8; 32]),
        }
        .abi_encode();
        precompile.call(&input, proposer, &mut provider).unwrap();

        // Vote so quorum is reached
        let input = IProtocolGovernance::voteCall {
            proposalId: 1,
            vote: 1,
        }
        .abi_encode();
        precompile.call(&input, proposer, &mut provider).unwrap();

        // queue from rando (not proposer, not validator) should fail
        let input = IProtocolGovernance::queueCall { proposalId: 1 }.abi_encode();
        let result = precompile.call(&input, rando, &mut provider);
        assert!(result.is_err(), "unauthorized queue should fail");
    }

    #[test]
    fn test_governance_precompile_emergency_pause_and_resume() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x44);

        // Seed validator so pause/resume work
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(sender),
                u64_to_u256(1),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // isPaused() -> false
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);

        // emergencyPause(reason)
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "pause failed: {:?}", result.err());

        // isPaused() -> true
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);

        // emergencyResume()
        let input = IProtocolGovernance::emergencyResumeCall {}.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "resume failed: {:?}", result.err());

        // isPaused() -> false
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);
    }

    #[test]
    fn test_governance_precompile_emergency_pause_multisig() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let val_a = Address::repeat_byte(0xAA);
        let val_b = Address::repeat_byte(0xBB);
        let val_c = Address::repeat_byte(0xCC);

        // Seed 3 validators
        for (addr, id) in [(val_a, 1), (val_b, 2), (val_c, 3)] {
            provider
                .sstore(
                    VALIDATOR_ADDRESS,
                    slot_validator_by_addr(addr),
                    u64_to_u256(id),
                )
                .unwrap();
        }
        // Seed validator count so threshold is computed correctly
        // threshold = ceil(3 * 6667 / 10000) = 3
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(3),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // First signature from val_a — not enough (need 3)
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(result.is_ok(), "first pause sig failed: {:?}", result.err());
        // Still not paused
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);

        // Second signature from val_b — still not enough
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, val_b, &mut provider);
        assert!(result.is_ok(), "second pause sig failed: {:?}", result.err());
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);

        // Third signature from val_c — threshold reached, pause executes
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, val_c, &mut provider);
        assert!(result.is_ok(), "third pause sig failed: {:?}", result.err());
        let input = IProtocolGovernance::isPausedCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);
    }

    #[test]
    fn test_governance_precompile_emergency_pause_rejected_when_already_paused() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let val_a = Address::repeat_byte(0xAA);

        // Seed 1 validator (threshold = ceil(1 * 6667 / 10000) = 1)
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(val_a),
                u64_to_u256(1),
            )
            .unwrap();
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(1),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // First call executes pause (threshold = 1)
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(result.is_ok(), "pause failed: {:?}", result.err());

        // Chain is now paused. Same validator calling again should be rejected
        // immediately (before recording any signature) so multi-sig state stays clean.
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(result.is_err(), "pause when already paused should fail");
    }

    #[test]
    fn test_governance_precompile_emergency_resume_rejected_when_not_paused() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let val_a = Address::repeat_byte(0xAA);

        // Seed 1 validator
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(val_a),
                u64_to_u256(1),
            )
            .unwrap();
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(1),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // Chain is not paused. Resume should be rejected immediately.
        let input = IProtocolGovernance::emergencyResumeCall {}.abi_encode();
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(
            result.is_err(),
            "resume when not paused should fail"
        );
    }

    #[test]
    fn test_governance_precompile_emergency_pause_double_sig_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let val_a = Address::repeat_byte(0xAA);

        // Seed 1 validator (threshold = ceil(1 * 6667 / 10000) = 1)
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(val_a),
                u64_to_u256(1),
            )
            .unwrap();
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(1),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // First call executes pause (threshold = 1)
        let input = IProtocolGovernance::emergencyPauseCall {
            reason: alloy_primitives::FixedBytes::<32>::from_slice(
                b"Emergency reason________________",
            ),
        }
        .abi_encode();
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(result.is_ok(), "pause failed: {:?}", result.err());

        // Same validator signing again in new round — epoch was incremented,
        // so this is a fresh round and should be allowed (but not enough if
        // threshold > 1). With 1 validator, threshold = 1, so this executes
        // another pause — but emergency_pause rejects double-pause.
        let result = precompile.call(&input, val_a, &mut provider);
        assert!(result.is_err(), "double pause should fail");
    }

    #[test]
    fn test_governance_precompile_cancel_proposal() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let proposer = Address::repeat_byte(0x44);
        let rando = Address::repeat_byte(0x55);

        // Seed proposer balance
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, proposer),
                u128_to_u256(PROPOSAL_DEPOSIT * 10),
            )
            .unwrap();
        // review_period=10 so proposal stays Pending
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"review_period"),
                u64_to_u256(10),
            )
            .unwrap();
        provider
            .sstore(
                GOVERNANCE_ADDRESS,
                slot_gov_config(b"voting_period"),
                u64_to_u256(100),
            )
            .unwrap();

        let mut precompile = GovernancePrecompile;

        // submitProposal
        let input = IProtocolGovernance::submitProposalCall {
            proposalType: 0,
            title: "Proposal".into(),
            description: "Desc".into(),
            executionData: alloy_primitives::Bytes::from_static(&[0xEEu8; 32]),
        }
        .abi_encode();
        precompile.call(&input, proposer, &mut provider).unwrap();

        // getProposal(1)
        let input = IProtocolGovernance::getProposalCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        // status at byte 31
        assert_eq!(result.bytes[31], 0); // Pending
        // proposalType at byte 63
        assert_eq!(result.bytes[63], 0); // ParameterChange
        // proposer address at bytes 76-95 (last 20 bytes of third word)
        assert_eq!(&result.bytes[76..96], proposer.as_slice());

        // cancelProposal from rando should fail
        let input = IProtocolGovernance::cancelProposalCall { proposalId: 1 }.abi_encode();
        let result = precompile.call(&input, rando, &mut provider);
        assert!(result.is_err(), "cancel by non-proposer should fail");

        // cancelProposal from proposer should succeed
        let input = IProtocolGovernance::cancelProposalCall { proposalId: 1 }.abi_encode();
        let result = precompile.call(&input, proposer, &mut provider);
        assert!(result.is_ok(), "cancel by proposer failed: {:?}", result.err());

        // getProposalStatus(1) should be Defeated (4)
        let input = IProtocolGovernance::getProposalStatusCall { proposalId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 4);
    }

    #[test]
    fn test_governance_precompile_view_rejects_invalid_proposal_id() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        let mut precompile = GovernancePrecompile;

        // getProposal(0) should fail — 0 is invalid
        let input = IProtocolGovernance::getProposalCall { proposalId: 0 }.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(result.is_err(), "getProposal(0) should fail");

        // getProposal(99) should fail — no proposals exist
        let input = IProtocolGovernance::getProposalCall { proposalId: 99 }.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(result.is_err(), "getProposal(99) should fail");

        // getProposalStatus(99) should fail
        let input = IProtocolGovernance::getProposalStatusCall { proposalId: 99 }.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(result.is_err(), "getProposalStatus(99) should fail");

        // getProposalVotes(99) should fail
        let input = IProtocolGovernance::getProposalVotesCall { proposalId: 99 }.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(result.is_err(), "getProposalVotes(99) should fail");
    }
}

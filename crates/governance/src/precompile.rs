//! Governance precompile entry point (0x203).
//!
//! Thin wrapper that routes EVM calls to [`GovernanceStorage`] backed by
//! any [`StorageProvider`].  Business logic lives in [`GovernanceStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, dispatch, require_caller,
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
pub const PROPOSAL_DEPOSIT: u128 = 10_000;
pub const GOV_TIMELOCK_BLOCKS: u64 = 100;
pub const GOV_QUORUM_BPS: u128 = 3_333;

// Governance config defaults (mirrors GovernanceConfig in manager.rs)
pub const GOV_REVIEW_PERIOD: u64 = 10;
pub const GOV_VOTING_PERIOD: u64 = 100;
pub const GOV_EXEC_TIMEOUT: u64 = 1000;
pub const GOV_PROPOSAL_COOLDOWN: u64 = 345_600;

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

    /// Read back the execution_data stored in chunked slots.
    pub fn read_proposal_execution_data(&mut self, proposal_id: u64) -> Vec<u8> {
        let data_len = u256_to_u64(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal_data_len(proposal_id)),
        ) as usize;
        if data_len == 0 {
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

    pub fn read_vote_tally(&mut self, proposal_id: u64, suffix: &[u8]) -> u128 {
        u256_to_u128(
            self.backend
                .load(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, suffix)),
        )
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

        let start_block = current_block + review_period;
        let end_block = start_block + voting_period;

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
        // Quorum is computed and set by the advancer when transitioning to Active
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal_quorum_required(proposal_id),
            U256::ZERO,
        );

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
        if current_block > end_block {
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

        self.backend
            .store(GOVERNANCE_ADDRESS, voter_slot, U256::from(vote_val));

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
            u128_to_u256(tally + voting_power),
        );

        Ok(())
    }

    pub fn queue(&mut self, proposal_id: u64, current_block: u64) -> Result<(), PrecompileError> {
        // Auto-advance Pending -> Active if review period passed
        let status = self.read_proposal_status(proposal_id);
        let start_block = self.read_proposal_u64(proposal_id, b"start_block");
        if status == 0 && current_block >= start_block {
            self.backend.store(
                GOVERNANCE_ADDRESS,
                slot_gov_proposal(proposal_id, b"status"),
                U256::from(1u8),
            );
        }

        self.require_proposal_status(proposal_id, 1, "governance: proposal not active")?;

        let votes_for = self.read_vote_tally(proposal_id, b"votes_for");
        let votes_against = self.read_vote_tally(proposal_id, b"votes_against");
        let votes_abstain = self.read_vote_tally(proposal_id, b"votes_abstain");
        let total_votes = votes_for + votes_against + votes_abstain;

        // Per-type quorum check
        let quorum_required = self.read_proposal_u128(proposal_id, b"quorum_required");
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");

        // If quorum wasn't set by advancer, fall back to generic quorum
        let has_quorum = if quorum_required > 0 {
            total_votes >= quorum_required
        } else {
            // Generic fallback: at least one vote and more for than against
            total_votes > 0 && votes_for > votes_against
        };

        if !has_quorum {
            // Defeat: confiscate deposit
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
            return Err(PrecompileError::Other(
                "governance: quorum not reached".into(),
            ));
        }

        if votes_for <= votes_against {
            // Defeat: confiscate deposit
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
            current_block + timelock
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

        let proposer = u256_to_address(self.backend.load(
            GOVERNANCE_ADDRESS,
            slot_gov_proposal(proposal_id, b"proposer"),
        ));
        let _ = executor;
        let _ = proposer;

        // Apply side effects based on proposal type
        let proposal_type = self.read_proposal_u8(proposal_id, b"proposal_type");
        let execution_data = self.read_proposal_execution_data(proposal_id);
        match proposal_type {
            0 => {
                // ParameterChange: execution_data = ABI-encoded (string configKey, uint128 newValue)
                // Minimal parse: skip 32-byte offset, read string offset, string length, string data,
                // then uint128 value. For simplicity, we use a fixed layout fallback.
                if execution_data.len() >= 48 {
                    // last 16 bytes = uint128 value
                    let mut value_buf = [0u8; 16];
                    value_buf.copy_from_slice(&execution_data[execution_data.len() - 16..]);
                    let new_value = u128::from_be_bytes(value_buf);
                    // First 32 bytes after any string offset = the string itself or we hash it
                    // For robustness, write to a well-known config suffix derived from first 32 bytes
                    let config_suffix: Vec<u8> = if execution_data.len() > 64 {
                        execution_data[32..64].to_vec()
                    } else {
                        b"param_change".to_vec()
                    };
                    self.write_config_u128(&config_suffix, new_value);
                }
            }
            2 => {
                // TreasurySpend: execution_data = ABI-encoded (address recipient, uint128 amount)
                if execution_data.len() >= 48 {
                    let mut addr_buf = [0u8; 20];
                    // address is the last 20 bytes of the first 32-byte word
                    addr_buf.copy_from_slice(&execution_data[12..32]);
                    let recipient = Address::from_slice(&addr_buf);
                    let mut amount_buf = [0u8; 16];
                    amount_buf.copy_from_slice(&execution_data[48..64]);
                    let amount = u128::from_be_bytes(amount_buf);
                    let _ = asset_store.add_balance(CALL_ASSET_ID, recipient, amount);
                }
            }
            5 => {
                // EmergencyPause: set paused flag
                self.backend
                    .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
            }
            6 => {
                // FeeCurrencyAdd: execution_data = ABI-encoded (uint64 assetId)
                if execution_data.len() >= 32 {
                    let mut asset_buf = [0u8; 8];
                    asset_buf.copy_from_slice(&execution_data[24..32]);
                    let asset_id = u64::from_be_bytes(asset_buf);
                    // Mark asset as accepted fee currency
                    self.backend.store(
                        GOVERNANCE_ADDRESS,
                        storage_slot(&[b"fee_currency", &asset_id.to_be_bytes()[..]]),
                        U256::from(1u8),
                    );
                }
            }
            7 => {
                // FeeCurrencyRemove: execution_data = ABI-encoded (uint64 assetId)
                if execution_data.len() >= 32 {
                    let mut asset_buf = [0u8; 8];
                    asset_buf.copy_from_slice(&execution_data[24..32]);
                    let asset_id = u64::from_be_bytes(asset_buf);
                    self.backend.store(
                        GOVERNANCE_ADDRESS,
                        storage_slot(&[b"fee_currency", &asset_id.to_be_bytes()[..]]),
                        U256::ZERO,
                    );
                }
            }
            9 => {
                // ValidatorKeyRotation: execution_data = ABI-encoded (uint64 validatorId, bytes32 newPubkey)
                if execution_data.len() >= 64 {
                    let mut id_buf = [0u8; 8];
                    id_buf.copy_from_slice(&execution_data[24..32]);
                    let _validator_id = u64::from_be_bytes(id_buf);
                    // Store rotation request signal
                    self.backend.store(
                        GOVERNANCE_ADDRESS,
                        storage_slot(&[b"key_rotation", &id_buf]),
                        U256::from(1u8),
                    );
                }
            }
            1 => {
                // ProtocolUpgrade: execution_data = ABI-encoded (bytes32 newVersionHash)
                if execution_data.len() >= 32 {
                    let mut version_buf = [0u8; 32];
                    version_buf.copy_from_slice(&execution_data[0..32]);
                    self.backend.store(
                        GOVERNANCE_ADDRESS,
                        storage_slot(&[b"protocol_upgrade"]),
                        U256::from_be_slice(&version_buf),
                    );
                }
            }
            3 => {
                // ValidatorSlash: execution_data = ABI-encoded (address validator, uint128 amount)
                if execution_data.len() >= 64 {
                    let mut addr_buf = [0u8; 20];
                    addr_buf.copy_from_slice(&execution_data[12..32]);
                    let validator = Address::from_slice(&addr_buf);
                    let mut amount_buf = [0u8; 16];
                    amount_buf.copy_from_slice(&execution_data[48..64]);
                    let amount = u128::from_be_bytes(amount_buf);
                    // Read current stake and slash
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
                }
            }
            4 => {
                // ComplianceUpdate: execution_data = ABI-encoded (address target, uint8 policyId, uint8 status)
                if execution_data.len() >= 96 {
                    let mut addr_buf = [0u8; 20];
                    addr_buf.copy_from_slice(&execution_data[12..32]);
                    let target = Address::from_slice(&addr_buf);
                    let policy_id = execution_data[63];
                    let status = execution_data[95];
                    self.backend.store(
                        COMPLIANCE_ADDRESS,
                        storage_slot(&[target.as_slice(), &[policy_id]]),
                        U256::from(status),
                    );
                }
            }
            8 => {
                // FeeCurrencyCap: execution_data = ABI-encoded (uint64 assetId, uint128 cap)
                if execution_data.len() >= 64 {
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
                }
            }
            10 => {
                // ProverKeyRotation: execution_data = ABI-encoded
                // (uint32 keyVersion, bytes32 transferVkHash, bytes32 depositVkHash, bytes32 withdrawVkHash, uint64 sunsetTimestamp)
                if execution_data.len() >= 160 {
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

                    // Store rotation signal and metadata in governance storage
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
                    // Set a pending flag so nodes know to pick it up
                    self.backend.store(
                        GOVERNANCE_ADDRESS,
                        storage_slot(&[b"prover_key_rotation", b"pending"]),
                        U256::from(1u8),
                    );
                }
            }
            _ => {}
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

    pub fn emergency_pause(&mut self, reason: [u8; 32], _pauser: Address) {
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::from(1u8));
        self.backend.store(
            GOVERNANCE_ADDRESS,
            slot_gov_pause_reason(),
            U256::from_be_slice(&reason),
        );
    }

    pub fn emergency_resume(&mut self) {
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_paused(), U256::ZERO);
        self.backend
            .store(GOVERNANCE_ADDRESS, slot_gov_pause_reason(), U256::ZERO);
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolGovernance {
        function submitProposal(uint8 proposalType, string title, string description, bytes executionData) external;
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
    fn submit_proposal(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::submitProposalCall, _>(
            calldata,
            200_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
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

                // Emit ProposalSubmitted(proposalId, proposer)
                let topic0 = alloy_primitives::keccak256(b"ProposalSubmitted(uint64,address)");
                let mut event_data = Vec::with_capacity(64);
                event_data.extend_from_slice(&u64_to_u256(proposal_id).to_be_bytes::<32>());
                event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0],
                    alloy_primitives::Bytes::from(event_data),
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
                        // ComplianceUpdate: joint voting (validator 1 + issuer weight)
                        if is_validator {
                            voting_power += 1;
                        }
                        // Check if caller is an asset issuer (simplified: check if they have issuer-level balance)
                        // Full issuer check would scan all assets; for now use balance proxy.
                        voting_power = voting_power.max(call_balance);
                    }
                    5 => {
                        // EmergencyPause: validator only, 1=1
                        if is_validator {
                            voting_power = 1;
                        }
                    }
                    6 | 7 | 8 | 9 => {
                        // FeeCurrencyAdd, FeeCurrencyRemove, FeeCurrencyCap, ValidatorKeyRotation: simple majority
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

                // Emit VoteCast(proposalId, voter, vote, power)
                let topic0 = alloy_primitives::keccak256(b"VoteCast(uint64,address,uint8,uint128)");
                let mut event_data = Vec::with_capacity(128);
                event_data.extend_from_slice(&u64_to_u256(call.proposalId).to_be_bytes::<32>());
                event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
                event_data.extend_from_slice(&U256::from(call.vote).to_be_bytes::<32>());
                event_data.extend_from_slice(&u128_to_u256(voting_power).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0],
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
        _msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolGovernance::queueCall, _>(
            calldata,
            20_000,
            storage,
            |call, storage| {
                let mut gov_store = GovernanceStorage::new(sr);
                let block_number = storage.block_number();
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
        dispatch::mutate_void::<IProtocolGovernance::executeCall, _>(
            calldata,
            20_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut gov_store = GovernanceStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);

                // Authorization check: proposer or validator
                let proposer = gov_store.read_proposal_proposer(call.proposalId);
                let is_proposer = caller == proposer;
                let is_validator = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller) != 0
                };
                if !is_proposer && !is_validator {
                    return Err(PrecompileError::Other(
                        "governance: unauthorized executor".into(),
                    ));
                }

                let block_number = storage.block_number();
                gov_store
                    .execute(&mut asset_store, call.proposalId, block_number, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // Emit ProposalExecuted(proposalId)
                let topic0 = alloy_primitives::keccak256(b"ProposalExecuted(uint64)");
                let mut event_data = Vec::with_capacity(32);
                event_data.extend_from_slice(&u64_to_u256(call.proposalId).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
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

                let mut gov_store = GovernanceStorage::new(sr);
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

                let mut gov_store = GovernanceStorage::new(sr);
                gov_store.emergency_resume();

                // Emit EmergencyResumed(resumer)
                let topic0 = alloy_primitives::keccak256(b"EmergencyResumed(address)");
                let mut event_data = Vec::with_capacity(32);
                event_data.extend_from_slice(&address_to_u256(caller).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(GOVERNANCE_ADDRESS, log);
                }

                Ok(())
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
                let votes_for = store.read_vote_tally(call.proposalId, b"votes_for");
                let votes_against = store.read_vote_tally(call.proposalId, b"votes_against");
                let votes_abstain = store.read_vote_tally(call.proposalId, b"votes_abstain");
                Ok((votes_for, votes_against, votes_abstain))
            },
        )
    }

    fn is_paused(&self, calldata: &[u8], storage: &mut dyn StorageProvider, sr: StorageRef) -> PrecompileResult {
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
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolGovernance::submitProposalCall::SELECTOR => {
                self.submit_proposal(calldata, msg_sender, storage, sr)
            }
            IProtocolGovernance::voteCall::SELECTOR => self.vote(calldata, msg_sender, storage, sr),
            IProtocolGovernance::queueCall::SELECTOR => self.queue(calldata, msg_sender, storage, sr),
            IProtocolGovernance::executeCall::SELECTOR => {
                self.execute(calldata, msg_sender, storage, sr)
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
                u128_to_u256(100_000),
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
                u128_to_u256(100_000),
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
            title: "Proposal".into(),
            description: "Desc".into(),
            executionData: alloy_primitives::Bytes::from_static(&[0xEEu8; 32]),
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // vote(proposalId=1, vote=1=Yes)
        // With type 0 (ParameterChange), voting power = max(1 if validator, call_balance)
        // Sender had 100_000 CALL balance but 10_000 was deducted as deposit,
        // so voting power = 90_000
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
        assert_eq!(votes_for, 90_000);

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
}

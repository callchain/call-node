//! GovernanceAdvancer — transient, stateless per-block proposal state machine.
//!
//! Replaces GovernanceManager's automatic advancement. Reads all proposal state
//! from EVM storage, applies transitions, writes results back, and returns
//! events for broadcasting.

use call_consensus::exec::state_accessors as sa;
use call_evm::provider::InMemoryStateProvider;
use call_governance::{GovernanceEvent, ProposalState};
use call_precompile::{storage::storage_slot, u64_to_u256};
use call_primitives::{Address, U256};
use call_protocol::CALL_ASSET_ID;

/// Total supply: 1B CALL * 10^18 (18 decimals)
const TOTAL_SUPPLY: u128 = 1_000_000_000_000_000_000_000_000_000u128;

/// Stateless governance proposal advancer.
#[derive(Debug, Default, Clone, Copy)]
pub struct GovernanceAdvancer;

impl GovernanceAdvancer {
    /// Advance the governance state machine for the current block.
    /// Reads from EVM, writes back, returns events.
    pub fn advance(&self, evm_state: &mut InMemoryStateProvider, current_block: u64) -> Vec<GovernanceEvent> {
        let mut events = Vec::new();

        let count = sa::read_gov_proposal_count(evm_state);
        if count == 0 {
            return events;
        }

        let timelock = sa::read_gov_config_timelock(evm_state);
        let timelock = if timelock == 0 { 100 } else { timelock };
        let exec_timeout = sa::read_gov_config_execution_timeout(evm_state);
        let exec_timeout = if exec_timeout == 0 { 1000 } else { exec_timeout };

        // Config for per-type quorum
        let validator_quorum_bps = sa::read_gov_config_validator_quorum_bps(evm_state);
        let validator_quorum_bps = if validator_quorum_bps == 0 { 6667 } else { validator_quorum_bps };
        let supply_quorum_bps = sa::read_gov_config_supply_quorum_bps(evm_state);
        let supply_quorum_bps = if supply_quorum_bps == 0 { 2000 } else { supply_quorum_bps };
        let treasury_quorum_bps = sa::read_gov_config_treasury_quorum_bps(evm_state);
        let treasury_quorum_bps = if treasury_quorum_bps == 0 { 2000 } else { treasury_quorum_bps };
        let simple_majority_bps = sa::read_gov_config_simple_majority_bps(evm_state);
        let simple_majority_bps = if simple_majority_bps == 0 { 5001 } else { simple_majority_bps };
        let emergency_pause_bps = sa::read_gov_config_validator_quorum_bps(evm_state); // reuses validator quorum

        let total_validators = sa::read_validator_count(evm_state);

        for proposal_id in 1..=count {
            let status = sa::read_gov_proposal_status(evm_state, proposal_id);

            match status {
                // Pending: check if review period passed, transition to Active
                0 => {
                    let review_end = sa::read_gov_proposal_review_end(evm_state, proposal_id);
                    let start_block = sa::read_gov_proposal_start_block(evm_state, proposal_id);
                    let review_done = if review_end > 0 {
                        current_block >= review_end
                    } else {
                        current_block >= start_block
                    };

                    if review_done {
                        sa::write_gov_proposal_status(evm_state, proposal_id, 1);

                        // Compute and store per-type quorum
                        let proposal_type = sa::read_gov_proposal_type(evm_state, proposal_id);
                        let quorum = Self::compute_quorum(
                            proposal_type,
                            total_validators,
                            validator_quorum_bps,
                            supply_quorum_bps,
                            treasury_quorum_bps,
                            simple_majority_bps,
                            emergency_pause_bps,
                        );
                        sa::write_gov_proposal_quorum_required(evm_state, proposal_id, quorum);

                        events.push(GovernanceEvent::ProposalAdvanced {
                            id: proposal_id,
                            from: ProposalState::Pending,
                            to: ProposalState::Active,
                        });
                    }
                }
                // Active: check if voting period ended
                1 => {
                    let end_block = sa::read_gov_proposal_end_block(evm_state, proposal_id);
                    if current_block > end_block {
                        let (votes_for, votes_against, _votes_abstain) =
                            sa::read_gov_proposal_votes(evm_state, proposal_id);
                        let total_votes = votes_for + votes_against;

                        let quorum_required = sa::read_gov_proposal_quorum_required(evm_state, proposal_id);
                        let proposal_type = sa::read_gov_proposal_type(evm_state, proposal_id);

                        // For validator-weighted proposals, quorum is validator count;
                        // for balance-weighted, quorum is CALL balance amount.
                        // If quorum_required wasn't set (0), use generic threshold.
                        let has_quorum = if quorum_required > 0 {
                            total_votes >= quorum_required
                        } else {
                            total_votes > 0 && votes_for > votes_against
                        };

                        if has_quorum && votes_for > votes_against {
                            // Auto-queue
                            let exec_block = if proposal_type == 5 {
                                // EmergencyPause skips timelock
                                current_block
                            } else {
                                current_block + timelock
                            };
                            sa::write_gov_proposal_status(evm_state, proposal_id, 2);
                            evm_state.set_storage(
                                call_governance::precompile::GOVERNANCE_ADDRESS,
                                sa::slot_gov_proposal(proposal_id, b"queued_at"),
                                u64_to_u256(current_block),
                            );
                            evm_state.set_storage(
                                call_governance::precompile::GOVERNANCE_ADDRESS,
                                sa::slot_gov_proposal(proposal_id, b"execution_block"),
                                u64_to_u256(exec_block),
                            );
                            events.push(GovernanceEvent::ProposalAdvanced {
                                id: proposal_id,
                                from: ProposalState::Active,
                                to: ProposalState::Queued,
                            });
                        } else {
                            // Auto-defeat: confiscate deposit
                            let proposer = sa::read_gov_proposal_proposer(evm_state, proposal_id);
                            if proposer != Address::ZERO {
                                evm_state.set_storage(
                                    call_governance::precompile::GOVERNANCE_ADDRESS,
                                    sa::slot_gov_proposal(proposal_id, b"deposit"),
                                    U256::ZERO,
                                );
                            }
                            sa::write_gov_proposal_status(evm_state, proposal_id, 4);
                            events.push(GovernanceEvent::ProposalDefeated { id: proposal_id });
                        }
                    }
                }
                // Queued: check if ready to execute or expired
                2 => {
                    let exec_block =
                        sa::read_gov_proposal_execution_block(evm_state, proposal_id);

                    // Expire check comes first (if past timeout)
                    if current_block > exec_block.saturating_add(exec_timeout) {
                        let proposer = sa::read_gov_proposal_proposer(evm_state, proposal_id);
                        if proposer != Address::ZERO {
                            evm_state.set_storage(
                                call_governance::precompile::GOVERNANCE_ADDRESS,
                                sa::slot_gov_proposal(proposal_id, b"deposit"),
                                U256::ZERO,
                            );
                        }
                        sa::write_gov_proposal_status(evm_state, proposal_id, 5);
                        events.push(GovernanceEvent::ProposalExpired { id: proposal_id });
                    } else if current_block >= exec_block {
                        // Execute: apply side effects, refund deposit, mark executed
                        let proposal_type = sa::read_gov_proposal_type(evm_state, proposal_id);
                        Self::apply_side_effects(evm_state, proposal_id, proposal_type);

                        let proposer = sa::read_gov_proposal_proposer(evm_state, proposal_id);
                        let deposit = sa::read_gov_proposal_deposit(evm_state, proposal_id);
                        if deposit > 0 && proposer != Address::ZERO {
                            let asset_id = call_protocol::CALL_ASSET_ID;
                            sa::add_balance_evm(evm_state, asset_id, proposer, deposit);
                            evm_state.set_storage(
                                call_governance::precompile::GOVERNANCE_ADDRESS,
                                sa::slot_gov_proposal(proposal_id, b"deposit"),
                                U256::ZERO,
                            );
                        }
                        sa::write_gov_proposal_status(evm_state, proposal_id, 3);
                        events.push(GovernanceEvent::ProposalExecuted {
                            id: proposal_id,
                            proposal_type: format!("{}", proposal_type),
                        });
                    }
                }
                _ => {}
            }
        }

        events
    }

    /// Compute quorum requirement for a proposal type.
    fn compute_quorum(
        proposal_type: u8,
        total_validators: u64,
        validator_quorum_bps: u32,
        supply_quorum_bps: u32,
        treasury_quorum_bps: u32,
        simple_majority_bps: u32,
        emergency_pause_bps: u32,
    ) -> u128 {
        let validator_quorum = if total_validators == 0 {
            0
        } else {
            (total_validators as u128 * validator_quorum_bps as u128).div_ceil(10_000)
        };
        let simple_majority = if total_validators == 0 {
            0
        } else {
            (total_validators as u128 * simple_majority_bps as u128).div_ceil(10_000)
        };
        let emergency_threshold = if total_validators == 0 {
            0
        } else {
            (total_validators as u128 * emergency_pause_bps as u128).div_ceil(10_000)
        };
        let supply_quorum = (TOTAL_SUPPLY * supply_quorum_bps as u128) / 10_000;
        let treasury_quorum = (TOTAL_SUPPLY * treasury_quorum_bps as u128) / 10_000;

        match proposal_type {
            0 => validator_quorum,               // ParameterChange
            1 => validator_quorum.max(supply_quorum), // ProtocolUpgrade
            2 => treasury_quorum,                // TreasurySpend
            3 => validator_quorum,               // ValidatorSlash
            4 => simple_majority,                // ComplianceUpdate
            5 => emergency_threshold,            // EmergencyPause
            6 | 7 | 8 | 9 => simple_majority,    // FeeCurrencyAdd/Remove/Cap, KeyRotation
            _ => simple_majority,
        }
    }

    /// Apply on-chain side effects for an executed proposal.
    /// Mirrors GovernanceStorage::execute() in crates/governance/src/precompile.rs.
    fn apply_side_effects(evm_state: &mut InMemoryStateProvider, proposal_id: u64, proposal_type: u8) {
        let execution_data = sa::read_gov_proposal_execution_data(evm_state, proposal_id);
        match proposal_type {
            0 => {
                // ParameterChange: execution_data = ABI-encoded (string configKey, uint128 newValue)
                if execution_data.len() >= 48 {
                    let mut value_buf = [0u8; 16];
                    value_buf.copy_from_slice(&execution_data[execution_data.len() - 16..]);
                    let new_value = u128::from_be_bytes(value_buf);
                    let config_suffix: Vec<u8> = if execution_data.len() > 64 {
                        execution_data[32..64].to_vec()
                    } else {
                        b"param_change".to_vec()
                    };
                    sa::write_gov_config_u128(evm_state, &config_suffix, new_value);
                }
            }
            2 => {
                // TreasurySpend: execution_data = ABI-encoded (address recipient, uint128 amount)
                if execution_data.len() >= 48 {
                    let mut addr_buf = [0u8; 20];
                    addr_buf.copy_from_slice(&execution_data[12..32]);
                    let recipient = Address::from_slice(&addr_buf);
                    let mut amount_buf = [0u8; 16];
                    amount_buf.copy_from_slice(&execution_data[48..64]);
                    let amount = u128::from_be_bytes(amount_buf);
                    sa::add_balance_evm(evm_state, CALL_ASSET_ID, recipient, amount);
                }
            }
            5 => {
                // EmergencyPause: set paused flag
                evm_state.set_storage(
                    call_governance::precompile::GOVERNANCE_ADDRESS,
                    sa::slot_gov_paused(),
                    U256::from(1u8),
                );
            }
            6 => {
                // FeeCurrencyAdd: execution_data = ABI-encoded (uint64 assetId)
                if execution_data.len() >= 32 {
                    let mut asset_buf = [0u8; 8];
                    asset_buf.copy_from_slice(&execution_data[24..32]);
                    let asset_id = u64::from_be_bytes(asset_buf);
                    evm_state.set_storage(
                        call_governance::precompile::GOVERNANCE_ADDRESS,
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
                    evm_state.set_storage(
                        call_governance::precompile::GOVERNANCE_ADDRESS,
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
                    evm_state.set_storage(
                        call_governance::precompile::GOVERNANCE_ADDRESS,
                        storage_slot(&[b"key_rotation", &id_buf]),
                        U256::from(1u8),
                    );
                }
            }
            // ProtocolUpgrade(1), ValidatorSlash(3), ComplianceUpdate(4), FeeCurrencyCap(8):
            // No additional on-chain side effects (matching precompile behavior).
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_consensus::exec::state_accessors as sa;
    use call_evm::provider::InMemoryStateProvider;
    use call_precompile::{u256_to_u128, u64_to_u256, GOVERNANCE_ADDRESS};
    use call_primitives::{Address, U256};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn setup_proposal(
        evm: &mut InMemoryStateProvider,
        proposal_id: u64,
        proposal_type: u8,
        status: u8,
        start_block: u64,
        end_block: u64,
        review_end: u64,
        votes_for: u128,
        votes_against: u128,
        votes_abstain: u128,
    ) {
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            U256::ZERO, // proposal_count slot
            u64_to_u256(proposal_id),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"status"),
            U256::from(status),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"start_block"),
            u64_to_u256(start_block),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"end_block"),
            u64_to_u256(end_block),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"proposal_type"),
            U256::from(proposal_type),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"review_end"),
            u64_to_u256(review_end),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"votes_for"),
            U256::from(votes_for),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"votes_against"),
            U256::from(votes_against),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"votes_abstain"),
            U256::from(votes_abstain),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"deposit"),
            U256::from(10_000u128),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(proposal_id, b"proposer"),
            call_precompile::address_to_u256(test_addr(10)),
        );
    }

    #[test]
    fn test_pending_to_active() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        setup_proposal(&mut evm, 1, 0, 0, 10, 110, 10, 0, 0, 0,
        );

        let advancer = GovernanceAdvancer;

        // Before review end: no transition
        let events = advancer.advance(&mut evm, 5);
        assert!(events.is_empty());
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 0);

        // After review end: Pending -> Active
        let events = advancer.advance(&mut evm, 15);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalAdvanced {
                id: 1,
                from: ProposalState::Pending,
                to: ProposalState::Active,
            }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 1);
        // Quorum should be set
        assert!(sa::read_gov_proposal_quorum_required(&evm, 1) > 0);
    }

    #[test]
    fn test_active_to_queued() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Active proposal with votes passing quorum
        setup_proposal(&mut evm, 1, 0, 1, 0, 100, 0, 1_000_000, 0, 0,
        );
        // Set quorum low enough
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);

        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 101);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalAdvanced {
                id: 1,
                from: ProposalState::Active,
                to: ProposalState::Queued,
            }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 2);
        assert!(sa::read_gov_proposal_execution_block(&evm, 1) > 101);
    }

    #[test]
    fn test_active_to_defeated() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Active proposal with no votes
        setup_proposal(&mut evm, 1, 0, 1, 0, 100, 0, 0, 0, 0,
        );

        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 101);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalDefeated { id: 1 }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 4);
        // Deposit confiscated
        assert_eq!(sa::read_gov_proposal_deposit(&evm, 1), 0);
    }

    #[test]
    fn test_queued_to_executed() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Queued proposal ready to execute
        setup_proposal(&mut evm, 1, 0, 2, 0, 100, 0, 1_000_000, 0, 0,
        );
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"queued_at"),
            u64_to_u256(40),
        );

        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 55);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalExecuted { id: 1, .. }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
    }

    #[test]
    fn test_queued_to_expired() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Queued proposal past execution timeout
        setup_proposal(&mut evm, 1, 0, 2, 0, 100, 0, 1_000_000, 0, 0,
        );
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        let exec_timeout = sa::read_gov_config_execution_timeout(&evm);
        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 50 + exec_timeout + 1);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalExpired { id: 1 }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 5);
        // Deposit confiscated
        assert_eq!(sa::read_gov_proposal_deposit(&evm, 1), 0);
    }

    #[test]
    fn test_queued_execute_before_expire() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Queued proposal: execution block reached, but also past timeout
        // Expire check should come FIRST
        setup_proposal(&mut evm, 1, 0, 2, 0, 100, 0, 1_000_000, 0, 0,
        );
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        let exec_timeout = sa::read_gov_config_execution_timeout(&evm);
        // Block is past both execution_block and timeout
        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 50 + exec_timeout + 1);

        // Should expire, not execute
        assert!(matches!(
            &events[0],
            GovernanceEvent::ProposalExpired { id: 1 }
        ));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 5);
    }

    #[test]
    fn test_emergency_pause_execute_sets_paused() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // EmergencyPause proposal (type 5) queued and ready
        setup_proposal(
            &mut evm, 1, 5, 2, 0, 100, 0, 1_000_000, 0, 0,
        );
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        assert!(!sa::read_gov_paused(&evm));

        let advancer = GovernanceAdvancer;
        advancer.advance(&mut evm, 55);

        assert!(sa::read_gov_paused(&evm));
        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
    }

    #[test]
    fn test_multiple_proposals_advance() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // Proposal 1: Pending -> Active
        setup_proposal(&mut evm, 1, 0, 0, 10, 110, 10, 0, 0, 0,
        );
        // Proposal 2: Active -> Queued
        setup_proposal(
            &mut evm, 2, 0, 1, 0, 50, 0, 1_000_000, 0, 0,
        );
        sa::write_gov_proposal_quorum_required(&mut evm, 2, 1);

        evm.set_storage(
            GOVERNANCE_ADDRESS,
            U256::ZERO, // proposal_count slot
            u64_to_u256(2),
        );

        let advancer = GovernanceAdvancer;
        let events = advancer.advance(&mut evm, 60);

        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 1); // Active
        assert_eq!(sa::read_gov_proposal_status(&evm, 2), 2); // Queued
        assert_eq!(events.len(), 2);
    }

    // ── Side-effect tests ───────────────────────────────────────────────

    fn store_execution_data(evm: &mut InMemoryStateProvider, proposal_id: u64, data: &[u8]) {
        let data_len = data.len() as u64;
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal_data_len(proposal_id),
            u64_to_u256(data_len),
        );
        for (chunk_idx, chunk) in data.chunks(32).enumerate() {
            let mut buf = [0u8; 32];
            buf[..chunk.len()].copy_from_slice(chunk);
            evm.set_storage(
                GOVERNANCE_ADDRESS,
                sa::slot_gov_proposal_data_chunk(proposal_id, chunk_idx as u64),
                U256::from_be_slice(&buf),
            );
        }
    }

    #[test]
    fn test_parameter_change_side_effect() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        setup_proposal(&mut evm, 1, 0, 2, 0, 100, 0, 1_000_000, 0, 0);
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        // ParameterChange execution_data: last 16 bytes = uint128 value (5000)
        let mut exec_data = vec![0u8; 64];
        exec_data[48..64].copy_from_slice(&5000u128.to_be_bytes());
        store_execution_data(&mut evm, 1, &exec_data);

        let advancer = GovernanceAdvancer;
        advancer.advance(&mut evm, 55);

        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
        // Config value should be written
        let config_val = u256_to_u128(evm.get_storage(&GOVERNANCE_ADDRESS, sa::slot_gov_config(b"param_change")));
        assert_eq!(config_val, 5000);
    }

    #[test]
    fn test_treasury_spend_side_effect() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        let recipient = test_addr(0xAB);
        setup_proposal(&mut evm, 1, 2, 2, 0, 100, 0, 1_000_000, 0, 0);
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        // TreasurySpend execution_data: bytes 12-32 = address, bytes 48-64 = amount
        let mut exec_data = vec![0u8; 64];
        exec_data[12..32].copy_from_slice(recipient.as_slice());
        exec_data[48..64].copy_from_slice(&7500u128.to_be_bytes());
        store_execution_data(&mut evm, 1, &exec_data);

        let advancer = GovernanceAdvancer;
        advancer.advance(&mut evm, 55);

        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
        let balance = sa::read_balance(&evm, CALL_ASSET_ID, recipient);
        assert_eq!(balance, 7500);
    }

    #[test]
    fn test_fee_currency_add_remove_side_effects() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        // FeeCurrencyAdd proposal (type 6) for asset_id = 42
        setup_proposal(&mut evm, 1, 6, 2, 0, 100, 0, 1_000_000, 0, 0);
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        // FeeCurrencyAdd execution_data: bytes 24-32 = uint64 assetId
        let mut exec_data = vec![0u8; 32];
        exec_data[24..32].copy_from_slice(&42u64.to_be_bytes());
        store_execution_data(&mut evm, 1, &exec_data);

        let advancer = GovernanceAdvancer;
        advancer.advance(&mut evm, 55);

        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
        let fee_slot = storage_slot(&[b"fee_currency", &42u64.to_be_bytes()[..]]);
        let val = evm.get_storage(&GOVERNANCE_ADDRESS, fee_slot).to_be_bytes::<32>()[31];
        assert_eq!(val, 1);

        // FeeCurrencyRemove proposal (type 7) for same asset
        setup_proposal(&mut evm, 2, 7, 2, 0, 100, 0, 1_000_000, 0, 0);
        sa::write_gov_proposal_quorum_required(&mut evm, 2, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(2, b"execution_block"),
            u64_to_u256(50),
        );
        store_execution_data(&mut evm, 2, &exec_data);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            U256::ZERO,
            u64_to_u256(2),
        );

        advancer.advance(&mut evm, 55);

        assert_eq!(sa::read_gov_proposal_status(&evm, 2), 3);
        let val = evm.get_storage(&GOVERNANCE_ADDRESS, fee_slot).to_be_bytes::<32>()[31];
        assert_eq!(val, 0);
    }

    #[test]
    fn test_key_rotation_side_effect() {
        let mut evm = InMemoryStateProvider::new();
        sa::seed_gov_config(&mut evm);
        sa::seed_validator(&mut evm, 1, test_addr(1), [1u8; 32], 1_000_000, 1);

        setup_proposal(&mut evm, 1, 9, 2, 0, 100, 0, 1_000_000, 0, 0);
        sa::write_gov_proposal_quorum_required(&mut evm, 1, 1);
        evm.set_storage(
            GOVERNANCE_ADDRESS,
            sa::slot_gov_proposal(1, b"execution_block"),
            u64_to_u256(50),
        );

        // ValidatorKeyRotation execution_data: bytes 24-32 = uint64 validatorId
        let mut exec_data = vec![0u8; 64];
        exec_data[24..32].copy_from_slice(&7u64.to_be_bytes());
        store_execution_data(&mut evm, 1, &exec_data);

        let advancer = GovernanceAdvancer;
        advancer.advance(&mut evm, 55);

        assert_eq!(sa::read_gov_proposal_status(&evm, 1), 3);
        let rotation_slot = storage_slot(&[b"key_rotation", &7u64.to_be_bytes()[..]]);
        let val = evm.get_storage(&GOVERNANCE_ADDRESS, rotation_slot).to_be_bytes::<32>()[31];
        assert_eq!(val, 1);
    }
}

//! GovernanceAdvancer — transient, stateless per-block proposal state machine.
//!
//! Replaces GovernanceManager's automatic advancement. Reads all proposal state
//! from EVM storage, applies transitions, writes results back, and returns
//! events for broadcasting.

use call_consensus::exec::state_accessors as sa;
use call_evm::EvmState;
use call_governance::{GovernanceEvent, ProposalState};
use call_precompiles::u64_to_u256;
use call_primitives::{Address, U256};

/// Stateless governance proposal advancer.
#[derive(Debug, Default, Clone, Copy)]
pub struct GovernanceAdvancer;

impl GovernanceAdvancer {
    /// Advance the governance state machine for the current block.
    /// Reads from EVM, writes back, returns events.
    pub fn advance(&self, evm_state: &mut EvmState, current_block: u64) -> Vec<GovernanceEvent> {
        let mut events = Vec::new();

        let count = sa::read_gov_proposal_count(evm_state);
        if count == 0 {
            return events;
        }

        let timelock = sa::read_gov_config_timelock(evm_state);
        let timelock = if timelock == 0 { 100 } else { timelock };
        let exec_timeout = sa::read_gov_config_execution_timeout(evm_state);
        let exec_timeout = if exec_timeout == 0 { 1000 } else { exec_timeout };
        let quorum_bps = sa::read_gov_config_quorum_bps(evm_state);
        let quorum_bps = if quorum_bps == 0 { 3_333u128 } else { quorum_bps };

        // Get active validator count for quorum calculation
        let active_validators = sa::read_validator_count(evm_state);
        let quorum_threshold = if active_validators == 0 {
            0
        } else {
            ((active_validators as u128 * quorum_bps) + 9_999) / 10_000
        };

        for proposal_id in 1..=count {
            let status = sa::read_gov_proposal_status(evm_state, proposal_id);

            match status {
                // Active: check if voting period ended
                1 => {
                    let end_block = sa::read_gov_proposal_end_block(evm_state, proposal_id);
                    if current_block > end_block {
                        let (votes_for, votes_against, _votes_abstain) =
                            sa::read_gov_proposal_votes(evm_state, proposal_id);
                        let total_votes = votes_for + votes_against;

                        if total_votes >= quorum_threshold && votes_for > votes_against {
                            // Auto-queue
                            sa::write_gov_proposal_status(evm_state, proposal_id, 2);
                            let exec_block = current_block + timelock;
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
                            // Auto-defeat
                            sa::write_gov_proposal_status(evm_state, proposal_id, 4);
                            events.push(GovernanceEvent::ProposalDefeated { id: proposal_id });
                        }
                    }
                }
                // Queued: check if ready to execute or expired
                2 => {
                    let exec_block =
                        sa::read_gov_proposal_execution_block(evm_state, proposal_id);
                    if current_block >= exec_block {
                        // Execute: refund deposit, mark executed
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
                            proposal_type: String::new(),
                        });
                    } else if current_block > exec_block + exec_timeout {
                        // Expire
                        sa::write_gov_proposal_status(evm_state, proposal_id, 5);
                        events.push(GovernanceEvent::ProposalExpired { id: proposal_id });
                    }
                }
                _ => {}
            }
        }

        events
    }
}

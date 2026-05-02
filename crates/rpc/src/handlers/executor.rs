//! NodeProposalExecutor and governance wiring.

use call_governance::{ProposalExecutor, Proposal};
use call_primitives::Address;
use crate::handlers::state::RpcState;
use std::sync::Arc;

/// Node-level proposal executor that dispatches to subsystems.
pub struct NodeProposalExecutor {
    pub state: Arc<RpcState>,
}

impl ProposalExecutor for NodeProposalExecutor {
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String> {
        use call_governance::ProposalType;

        match &proposal.proposal_type {
            ProposalType::ProtocolUpgrade { activation_block, changelog } => {
                // Parse version from changelog (format: "vX.Y.Z")
                let version = parse_version(changelog).unwrap_or_else(|| {
                    call_primitives::ProtocolVersion::new(1, 0, 0)
                });
                let current = self.state.get_current_block();
                let mut fm = self.state.fork_manager.write().map_err(|_| "fork lock poisoned".to_string())?;
                fm.schedule_governance_upgrade(version, *activation_block, proposal.id, current)
                    .map_err(|e| format!("fork upgrade: {e:?}"))?;
                tracing::info!(version = ?version, height = activation_block, proposal_id = proposal.id, "governance protocol upgrade scheduled");
            }
            ProposalType::ValidatorSlash { validator_id, reason } => {
                // Slash validator in EVM storage (set stake to 0 and status to 0)
                let mut evm_state = self.state.evm_state.write().map_err(|_| "evm lock poisoned".to_string())?;
                let addr = call_consensus::exec::evm_instructions::read_validator_addr(
                    &evm_state, *validator_id as u64);
                if addr == call_primitives::Address::ZERO {
                    return Err(format!("validator {validator_id} not found in EVM storage"));
                }
                let slashed = call_consensus::exec::evm_instructions::read_validator_stake(
                    &evm_state, addr);
                call_consensus::exec::evm_instructions::remove_validator_evm(
                    &mut evm_state, addr);
                tracing::info!(validator_id, reason, slashed_amount = slashed, "validator slashed via governance");
            }
            ProposalType::EmergencyPause { reason } => {
                // Already handled by GovernanceManager.apply_proposal
                tracing::info!(reason, "emergency pause confirmed via executor");
            }
            ProposalType::ParameterChange { param_id, new_value } => {
                // Parse new_value as JSON
                match serde_json::from_str::<serde_json::Value>(new_value) {
                    Ok(val) => {
                        // Governance-internal config updates (governance.*, protocol.*)
                        // are handled by GovernanceManager::apply_proposal.
                        // The executor only applies cross-system parameter changes.
                        if param_id.starts_with("governance.") || param_id.starts_with("protocol.") {
                            tracing::info!(param_id, new_value, "governance-internal param change already applied");
                        } else if param_id.starts_with("consensus.") {
                            let mut cp = self.state.consensus_params.write().map_err(|_| "consensus params lock poisoned".to_string())?;
                            if let Some(v) = val.get("max_validators").and_then(|v| v.as_u64()) {
                                cp.max_validators = v as u32;
                            }
                            if let Some(v) = val.get("subset_size").and_then(|v| v.as_u64()) {
                                cp.subset_size = v as u32;
                            }
                            if let Some(v) = val.get("block_time_millis").and_then(|v| v.as_u64()) {
                                cp.block_time_millis = v;
                            }
                            if let Some(v) = val.get("slashing_window").and_then(|v| v.as_u64()) {
                                cp.slashing_window = v;
                            }
                            if let Some(v) = val.get("oracle_request_delay_ms").and_then(|v| v.as_u64()) {
                                cp.oracle_request_delay_ms = v;
                            }
                            if let Some(v) = val.get("epoch_length").and_then(|v| v.as_u64()) {
                                cp.epoch_length = v;
                            }
                            tracing::info!(param_id, new_value, "consensus params updated via executor");
                        } else if param_id.starts_with("validator.") {
                            let mut cp = self.state.consensus_params.write().map_err(|_| "consensus params lock poisoned".to_string())?;
                            if let Some(v) = val.get("min_self_stake").and_then(|v| v.as_u64()) {
                                cp.min_self_stake = v as u128;
                            }
                            if let Some(v) = val.get("unbonding_period_blocks").and_then(|v| v.as_u64()) {
                                cp.unbonding_period_blocks = v;
                            }
                            if let Some(v) = val.get("offline_slash_rate_bps").and_then(|v| v.as_u64()) {
                                cp.offline_slash_rate_bps = v as u128;
                            }
                            if let Some(v) = val.get("key_rotation_grace_blocks").and_then(|v| v.as_u64()) {
                                cp.key_rotation_grace_blocks = v;
                            }
                            if let Some(v) = val.get("churn_limit_quotient").and_then(|v| v.as_u64()) {
                                cp.churn_limit_quotient = v;
                            }
                            if let Some(v) = val.get("min_churn_limit").and_then(|v| v.as_u64()) {
                                cp.min_churn_limit = v;
                            }
                            if let Some(v) = val.get("safety_ratio_num").and_then(|v| v.as_u64()) {
                                cp.safety_ratio_num = v as u32;
                            }
                            if let Some(v) = val.get("safety_ratio_den").and_then(|v| v.as_u64()) {
                                cp.safety_ratio_den = v as u32;
                            }
                            if let Some(v) = val.get("unbonding_slash_extend").and_then(|v| v.as_u64()) {
                                cp.unbonding_slash_extend = v as u32;
                            }
                            if let Some(v) = val.get("max_unbonding_multiplier").and_then(|v| v.as_u64()) {
                                cp.max_unbonding_multiplier = v as u32;
                            }
                            tracing::info!(param_id, new_value, "validator params updated via executor (consensus_params)");
                        } else if param_id.starts_with("oracle.") {
                            let mut oracle = self.state.oracle.write().map_err(|_| "oracle lock poisoned".to_string())?;
                            let mut config = oracle.config.clone();
                            if let Some(v) = val.get("update_interval").and_then(|v| v.as_u64()) {
                                config.update_interval = v;
                            }
                            if let Some(v) = val.get("outlier_threshold_bps").and_then(|v| v.as_u64()) {
                                config.outlier_threshold_bps = v;
                            }
                            if let Some(v) = val.get("outlier_tolerance").and_then(|v| v.as_u64()) {
                                config.outlier_tolerance = v as u32;
                            }
                            if let Some(v) = val.get("twap_window_secs").and_then(|v| v.as_u64()) {
                                config.twap_window_secs = v;
                            }
                            if let Some(v) = val.get("staleness_secs").and_then(|v| v.as_u64()) {
                                config.staleness_secs = v;
                            }
                            if let Some(v) = val.get("min_data_sources").and_then(|v| v.as_u64()) {
                                config.min_data_sources = v as usize;
                            }
                            oracle.update_config(config);
                            tracing::info!(param_id, new_value, "oracle config updated via executor");
                        } else if param_id.starts_with("fee_currency.") {
                            let mut fcr = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                            if let Some(v) = val.get("min_market_cap_usd").and_then(|v| v.as_u64()) {
                                fcr.min_market_cap_usd = v as u128;
                            }
                            if let Some(v) = val.get("stablecoin_cap_bps").and_then(|v| v.as_u64()) {
                                fcr.stablecoin_cap_bps = v as u32;
                            }
                            tracing::info!(param_id, new_value, "fee currency params updated via executor");
                        } else {
                            // Standard fee params updates
                            let mut fp = self.state.fee_params.write().map_err(|_| "fee params lock poisoned".to_string())?;
                            if let Some(v) = val.get("base_fee").and_then(|v| v.as_u64()) {
                                fp.base_fee = v as u128;
                            }
                            if let Some(v) = val.get("target_gas_per_block").and_then(|v| v.as_u64()) {
                                fp.target_gas_per_block = v;
                            }
                            if let Some(v) = val.get("max_gas_per_block").and_then(|v| v.as_u64()) {
                                fp.max_gas_per_block = v;
                            }
                            if let Some(v) = val.get("oracle_fee_share_bps").and_then(|v| v.as_u64()) {
                                fp.oracle_fee_share_bps = v as u16;
                            }
                            tracing::info!(param_id, new_value, "parameter change applied via executor");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(param_id, error = %e, "failed to parse parameter change value as JSON, skipping");
                    }
                }
            }
            ProposalType::TreasurySpend { recipient, amount, asset_id } => {
                // Already applied by GovernanceManager.apply_proposal
                tracing::info!(asset_id, amount, ?recipient, "treasury spend confirmed via executor");
            }
            ProposalType::ComplianceUpdate { asset_id, new_policy } => {
                // Map policy u8 to CompliancePolicy and set asset compliance
                let policy = match *new_policy {
                    0 => 0, // None
                    1 => 1, // OfacBlacklist
                    2 => 2, // KycRequired
                    3 => 3, // Whitelist
                    4 => 4, // Custom
                    _ => return Err(format!("unknown compliance policy id: {new_policy}")),
                };
                // Update the asset compliance policy in EVM storage
                let mut evm = self.state.evm_state.write().map_err(|_| "evm lock poisoned".to_string())?;
                call_consensus::exec::evm_instructions::seed_asset_compliance(&mut evm, *asset_id, policy);
                tracing::info!(asset_id, new_policy, "compliance update applied via executor");
            }
            ProposalType::FeeCurrencyAdd { asset_id, name, oracle_price_key } => {
                let key_bytes: Option<[u8; 32]> = if oracle_price_key.is_empty() {
                    None
                } else {
                    let mut arr = [0u8; 32];
                    let bytes = hex::decode(oracle_price_key.trim_start_matches("0x")).unwrap_or_default();
                    let len = bytes.len().min(32);
                    arr[..len].copy_from_slice(&bytes[..len]);
                    Some(arr)
                };
                let current_block = self.state.get_current_block();
                let entry = call_protocol::FeeCurrencyEntry {
                    asset_id: *asset_id,
                    name: name.clone(),
                    decimals: 18,
                    oracle_price_key: key_bytes,
                    added_at_block: current_block,
                    added_by_proposal: proposal.id,
                };
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.add_fee_currency(entry, proposal.id)
                    .map_err(|e| format!("failed to add fee currency: {e}"))?;
                tracing::info!(asset_id, name, "fee currency registered via governance");
            }
            ProposalType::FeeCurrencyRemove { asset_id, grace_period_blocks } => {
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.remove_fee_currency(*asset_id, *grace_period_blocks)
                    .map_err(|e| format!("failed to remove fee currency: {e}"))?;
                tracing::info!(asset_id, grace_period_blocks, "fee currency removed via governance");
            }
            ProposalType::FeeCurrencyCap { new_cap_bps } => {
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.stablecoin_cap_bps = *new_cap_bps;
                tracing::info!(new_cap_bps, "fee currency cap updated via governance");
            }
            ProposalType::ValidatorKeyRotation { validator_id, old_pubkey, new_pubkey, signature } => {
                // Verify the old key signed the rotation request
                let msg = {
                    let mut buf = Vec::with_capacity(64);
                    buf.extend_from_slice(old_pubkey);
                    buf.extend_from_slice(new_pubkey);
                    buf
                };
                let msg_hash = call_crypto::keccak256(&msg);

                // Recover signer address from signature
                if signature.len() != 65 {
                    return Err("rotation signature must be 65 bytes".into());
                }
                let sig_arr: [u8; 65] = signature.as_slice().try_into().map_err(|_| "invalid signature length")?;
                let recovered = call_crypto::recover_secp256k1_signer(&msg_hash, &sig_arr)
                    .map_err(|e| format!("failed to recover signer from rotation signature: {e}"))?;

                // Look up the validator's current address from EVM storage
                let validator_addr = {
                    let evm_state = self.state.evm_state.read().map_err(|_| "evm lock poisoned".to_string())?;
                    let addr = call_consensus::exec::evm_instructions::read_validator_addr(
                        &evm_state, *validator_id as u64);
                    if addr == call_primitives::Address::ZERO {
                        return Err(format!("validator {validator_id} not found in EVM storage"));
                    }
                    addr
                };

                // The recovered address must match the validator's address
                if recovered != validator_addr {
                    return Err(format!("rotation signature from wrong address: expected {validator_addr:?}, got {recovered:?}"));
                }

                // Rotate the key in EVM storage
                let mut evm_state = self.state.evm_state.write().map_err(|_| "evm lock poisoned".to_string())?;
                call_consensus::exec::evm_instructions::rotate_validator_key_evm(
                    &mut evm_state, validator_addr, *new_pubkey);
                tracing::info!(validator_id, "validator key rotated via governance in EVM storage");
            }
        }

        Ok(())
    }
}

/// Parse a version string like "v1.2.3" into ProtocolVersion.
fn parse_version(s: &str) -> Option<call_primitives::ProtocolVersion> {
    let s = s.trim_start_matches('v');
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse::<u16>().ok()?;
        let minor = parts[1].parse::<u16>().ok()?;
        let patch = parts.get(2).and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
        Some(call_primitives::ProtocolVersion::new(major, minor, patch))
    } else {
        None
    }
}

/// Wire the governance executor so proposals can trigger real side effects.
/// Called after RpcState is wrapped in Arc.
pub fn wire_governance_executor(state: &Arc<RpcState>) {
    let executor = Arc::new(NodeProposalExecutor {
        state: Arc::clone(state),
    });
    let balance_source = {
        let state = Arc::clone(state);
        Arc::new(move |addr: Address| {
            state.evm_state.read().ok().map(|s| {
                call_consensus::exec::evm_instructions::read_balance(&s, call_protocol::CALL_ASSET_ID, addr)
            }).unwrap_or(0)
        })
    };
    if let Ok(mut gov) = state.governance.write() {
        gov.executor = Some(executor);
        gov.balance_source = Some(balance_source);
    }
}

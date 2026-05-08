//! Callchain extension RPC endpoints (per spec §11.2)
//!
//! Architecture: all state-mutating operations go through EVM precompiles.
//! Read-only endpoints remain as individual `call_*` methods.

use crate::handlers::helpers::{
    db_error, internal_error, invalid_params, method_not_available, resource_unavailable,
};
use crate::handlers::state::RpcState;
use call_consensus::exec::state_accessors;
use call_primitives::Address;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;
use std::sync::Arc;

/// Register Callchain extension RPC methods
pub fn register_callchain_rpc(
    module: &mut RpcModule<Arc<RpcState>>,
) -> Result<(), ErrorObjectOwned> {
    // ── Read-Only Query Endpoints ──────────────────────────────────

    // call_assetInfo
    module
        .register_async_method("call_assetInfo", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            match state.get_asset_info(asset_id) {
                Some(info) => Ok::<_, ErrorObjectOwned>(
                    serde_json::to_value(info).map_err(|e| internal_error(e.to_string()))?,
                ),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_protocolBalance
    module
        .register_async_method("call_protocolBalance", |params, state, _ctx| async move {
            let (asset_id, address): (u64, String) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address
                .parse::<Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let balance = state.get_balance(asset_id, &addr);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "address": address,
                "balance": balance.to_string(),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getNonce
    module
        .register_async_method("call_getNonce", |params, state, _ctx| async move {
            let address: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address
                .parse::<Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let nonce = state.get_nonce(&addr);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "address": address,
                "nonce": nonce,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_compliancePolicy
    module
        .register_async_method("call_compliancePolicy", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let policy = state.get_compliance_policy(asset_id);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "compliancePolicy": policy,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_totalBalance
    module
        .register_async_method("call_totalBalance", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let total = state.get_total_balance(asset_id);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "totalBalance": total.to_string(),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Agent Read Endpoints ───────────────────────────────────────

    // call_agentInfo
    module
        .register_async_method("call_agentInfo", |params, state, _ctx| async move {
            let agent_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            match state.get_agent_info(agent_id) {
                Some(info) => Ok::<_, ErrorObjectOwned>(
                    serde_json::to_value(info).map_err(|e| internal_error(e.to_string()))?,
                ),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_agentBalance
    module
        .register_async_method("call_agentBalance", |params, state, _ctx| async move {
            let agent_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let balance = state.get_agent_total_balance(agent_id);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "agentId": agent_id,
                "balance": balance.to_string(),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_agentHistory
    module
        .register_async_method("call_agentHistory", |_params, state, _ctx| async move {
            let agent_id: u64 = _params.one().map_err(|e| invalid_params(e.to_string()))?;
            match state.get_agent_info(agent_id) {
                Some(info) => {
                    let receipts = state.get_all_receipts();
                    let history: Vec<serde_json::Value> = receipts
                        .iter()
                        .filter(|r| r.gas_payer == info.owner)
                        .map(|r| {
                            serde_json::json!({
                                "txHash": format!("0x{}", hex::encode(r.tx_hash)),
                                "status": format!("{:?}", r.status),
                                "gasUsed": r.gas_used.to_string(),
                                "feeAmount": r.fee_amount.to_string(),
                            })
                        })
                        .collect();
                    Ok::<_, ErrorObjectOwned>(serde_json::json!({
                        "agentId": agent_id,
                        "owner": format!("{:?}", info.owner),
                        "history": history,
                    }))
                }
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "agentId": agent_id,
                    "owner": null,
                    "history": Vec::<serde_json::Value>::new(),
                })),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Shielded Read Endpoints ────────────────────────────────────

    // call_shieldedDepositProve
    module
        .register_async_method("call_shieldedDepositProve", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(method_not_available(
                "shielded deposit proving requires a local prover — use `call-cli shielded deposit-prove <args>` or run a dedicated proving service with `--prover-mode deposit`",
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedTransferProve
    module
        .register_async_method("call_shieldedTransferProve", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(method_not_available(
                "shielded transfer proving requires a local prover — use `call-cli shielded transfer-prove <args>` or run a dedicated proving service with `--prover-mode transfer`",
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedBalance
    module
        .register_async_method("call_shieldedBalance", |params, state, _ctx| async move {
            let call_obj: serde_json::Value =
                params.one().map_err(|e| invalid_params(e.to_string()))?;
            let vk_hex = call_obj
                .get("viewingKey")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'viewingKey' field"))?;
            let vk_bytes = hex::decode(vk_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid viewing key: {e}")))?;
            if vk_bytes.len() < 32 {
                return Err(invalid_params(
                    "viewing key must be at least 32 bytes",
                ));
            }
            let mut ivk = [0u8; 32];
            ivk.copy_from_slice(&vk_bytes[..32]);
            let fvk = if vk_bytes.len() >= 64 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&vk_bytes[32..64]);
                arr
            } else {
                [0u8; 32]
            };
            let _vk = call_shielded::ViewingKey {
                incoming_view_key: ivk,
                full_view_key: fvk,
            };
            let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                .map_err(|e| db_error(format!("db error: {e}")))?;
            let merkle_root = state_accessors::read_shielded_merkle_root(&provider);
            // Note balances require the full note registry (not in EVM storage).
            // Return merkle root only; use a local shielded node for balance queries.
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "balance": "0",
                "noteCount": 0,
                "merkleRoot": format!("{:?}", merkle_root),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedTreeState
    module
        .register_async_method(
            "call_shieldedTreeState",
            |_params, state, _ctx| async move {
                let tree_state = state.get_shielded_tree_state();
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "merkleRoot": format!("{:?}", tree_state.merkle_root),
                    "leafCount": tree_state.leaf_count,
                    "nullifierCount": tree_state.nullifier_count,
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Receipt / Log Read Endpoints ───────────────────────────────

    // call_getTransactionReceipt
    module
        .register_async_method(
            "call_getTransactionReceipt",
            |params, state, _ctx| async move {
                let tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
                let hash = tx_hash
                    .parse::<alloy_primitives::B256>()
                    .map_err(|e| invalid_params(e.to_string()))?;
                match state.get_receipt(&hash) {
                    Some(receipt) => Ok::<_, ErrorObjectOwned>(Some(serde_json::json!({
                        "txHash": format!("{:?}", receipt.tx_hash),
                        "status": format!("{:?}", receipt.status),
                        "gasUsed": receipt.gas_used.to_string(),
                        "gasPayer": format!("{:?}", receipt.gas_payer),
                        "feeAmount": receipt.fee_amount.to_string(),
                    }))),
                    None => Ok::<_, ErrorObjectOwned>(None),
                }
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getBlockReceipts
    module
        .register_async_method("call_getBlockReceipts", |params, state, _ctx| async move {
            let block: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let receipts = state.get_receipts_by_block(block);
            let result: Vec<serde_json::Value> = receipts
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "txHash": format!("{:?}", r.tx_hash),
                        "status": format!("{:?}", r.status),
                        "gasUsed": r.gas_used.to_string(),
                    })
                })
                .collect();
            Ok::<_, ErrorObjectOwned>(result)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getLogs
    module
        .register_async_method("call_getLogs", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let addresses: Vec<Address> = call_obj.get("address")
                .map(|v| match v {
                    serde_json::Value::String(s) => vec![s.parse::<Address>()].into_iter().filter_map(|r| r.ok()).collect(),
                    serde_json::Value::Array(arr) => arr.iter()
                        .filter_map(|x| x.as_str().and_then(|s| s.parse::<Address>().ok()))
                        .collect(),
                    _ => vec![],
                })
                .unwrap_or_default();
            let receipts = state.get_receipts_by_block(0);
            let logs: Vec<serde_json::Value> = receipts
                .iter()
                .flat_map(|r| r.logs.iter().map(|log| (r.tx_hash, log)))
                .filter(|(_, log)| {
                    addresses.is_empty() || addresses.contains(&log.address)
                })
                .map(|(tx_hash, log)| serde_json::json!({
                    "transactionHash": format!("0x{}", hex::encode(tx_hash)),
                    "address": format!("{:?}", log.address),
                    "topics": log.topics.iter().map(|t| format!("0x{}", hex::encode(t.as_slice()))).collect::<Vec<_>>(),
                    "data": format!("0x{}", hex::encode(&log.data)),
                }))
                .collect();
            Ok::<_, ErrorObjectOwned>(logs)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getTxByReference
    module
        .register_async_method("call_getTxByReference", |params, state, _ctx| async move {
            let ref_str: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let hash = ref_str
                .parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            match state.get_receipt(&hash) {
                Some(receipt) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "txHash": format!("0x{}", hex::encode(receipt.tx_hash)),
                    "status": format!("{:?}", receipt.status),
                    "gasUsed": receipt.gas_used.to_string(),
                    "gasPayer": format!("{:?}", receipt.gas_payer),
                    "feeCurrency": format!("{:?}", receipt.fee_currency),
                    "feeAmount": receipt.fee_amount.to_string(),
                })),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Rollback / Fork Read Endpoints ─────────────────────────────

    // call_getRollbackHistory
    module
        .register_async_method("call_getRollbackHistory", |_params, state, _ctx| async move {
            let fm = state.fork_manager.read().map_err(|_| resource_unavailable("lock poisoned"))?;
            let history: Vec<serde_json::Value> = fm.rollback_history()
                .iter()
                .map(|r| serde_json::json!({
                    "targetHeight": r.target_height,
                    "targetVersion": format!("{}.{}.{}", r.target_version.major, r.target_version.minor, r.target_version.patch),
                    "signatureCount": r.signature_count,
                    "totalValidators": r.total_validators,
                    "executedAtHeight": r.executed_at_height,
                }))
                .collect();
            Ok::<_, ErrorObjectOwned>(serde_json::json!({ "history": history }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getScheduledUpgrades
    module
        .register_async_method("call_getScheduledUpgrades", |_params, state, _ctx| async move {
            let fm = state.fork_manager.read().map_err(|_| resource_unavailable("lock poisoned"))?;
            let current_block = *state.current_block.read().map_err(|_| resource_unavailable("lock poisoned"))?;
            let upgrades: Vec<serde_json::Value> = fm.scheduled_upgrades()
                .iter()
                .filter(|u| !u.applied)
                .map(|u| serde_json::json!({
                    "version": format!("{}.{}.{}", u.version.major, u.version.minor, u.version.patch),
                    "activationHeight": u.activation_height,
                    "proposalId": u.proposal_id,
                    "approvedAtHeight": u.approved_at_height,
                }))
                .collect();
            let next = fm.next_upgrade(current_block).map(|u| serde_json::json!({
                "version": format!("{}.{}.{}", u.version.major, u.version.minor, u.version.patch),
                "activationHeight": u.activation_height,
            }));
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "upgrades": upgrades,
                "nextUpgrade": next,
                "currentVersion": format!("{}.{}.{}", fm.current_version.major, fm.current_version.minor, fm.current_version.patch),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Light Client Read Endpoints ────────────────────────────────

    // call_lightVerifyBlockHeader
    module
        .register_async_method(
            "call_lightVerifyBlockHeader",
            |params, state, _ctx| async move {
                let call_obj: serde_json::Value =
                    params.one().map_err(|e| invalid_params(e.to_string()))?;
                let header_json = call_obj
                    .get("header")
                    .ok_or_else(|| invalid_params("missing 'header' field"))?;
                let sigs_json = call_obj
                    .get("signatures")
                    .ok_or_else(|| invalid_params("missing 'signatures' field"))?;
                let header: call_consensus::BlockHeader =
                    serde_json::from_value(header_json.clone())
                        .map_err(|e| invalid_params(format!("invalid header: {e}")))?;
                let block_hash = header.hash();
                let sigs_array = sigs_json
                    .get("signatures")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| invalid_params("missing signatures array"))?;
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let validators = state_accessors::read_validator_addresses(&provider);
                let total = validators.len() as u32;
                let quorum = (2 * total as usize).div_ceil(3).max(1);
                let block_hash_bytes = block_hash.as_slice();
                let mut valid_count = 0;
                for entry in sigs_array {
                    if let Some(validator_id) = entry.get(0).and_then(|v| v.as_u64()) {
                        let addr = state_accessors::read_validator_addr(&provider, validator_id);
                        if addr != Address::ZERO {
                            let status = state_accessors::read_validator_status(&provider, addr);
                            if status != 0 {
                                if let Some(sig_hex) = entry.get(2).and_then(|v| v.as_str()) {
                                    if let Ok(sig_bytes) =
                                        hex::decode(sig_hex.trim_start_matches("0x"))
                                    {
                                        if sig_bytes.len() == 64 {
                                            let mut sig = [0u8; 64];
                                            sig.copy_from_slice(&sig_bytes);
                                            let pk = state_accessors::read_validator_pubkey(
                                                &provider, addr,
                                            );
                                            if call_crypto::ed25519_verify(
                                                &pk,
                                                &sig,
                                                block_hash_bytes,
                                            )
                                            .is_ok()
                                            {
                                                valid_count += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let valid =
                    valid_count >= quorum && header.timestamp_millis > 0 && header.proposer > 0;
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "valid": valid,
                    "height": header.height,
                    "hash": format!("{block_hash:?}"),
                    "signatureCount": valid_count,
                    "quorum": quorum,
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetBalanceProof
    module
        .register_async_method(
            "call_lightGetBalanceProof",
            |params, state, _ctx| async move {
                let (asset_id, address_str): (u64, String) =
                    params.parse().map_err(|e| invalid_params(e.to_string()))?;
                let address = address_str
                    .parse::<Address>()
                    .map_err(|e| invalid_params(e.to_string()))?;
                let balance = state.get_balance(asset_id, &address);
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let merkle_root = state_accessors::read_shielded_merkle_root(&provider);
                let leaf_count = state_accessors::read_shielded_commitment_count(&provider);
                // Full note proofs require the Merkle tree structure (not in EVM storage).
                let state_leaf =
                    call_crypto::keccak256(format!("{asset_id}:{address:?}:{balance}").as_bytes());
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "assetId": asset_id,
                    "address": address_str,
                    "balance": balance.to_string(),
                    "stateCommitment": format!("0x{}", hex::encode(state_leaf)),
                    "merkleRoot": format!("0x{}", hex::encode(merkle_root.as_slice())),
                    "leafCount": leaf_count,
                    "noteProofs": Vec::<serde_json::Value>::new(),
                    "blockNumber": state.get_current_block(),
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetTransactionProof
    module
        .register_async_method(
            "call_lightGetTransactionProof",
            |params, state, _ctx| async move {
                let tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
                let hash = tx_hash
                    .parse::<alloy_primitives::B256>()
                    .map_err(|e| invalid_params(e.to_string()))?;
                match state.get_receipt(&hash) {
                    Some(receipt) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                        "txHash": format!("0x{}", hex::encode(receipt.tx_hash)),
                        "blockNumber": state.get_current_block(),
                        "status": format!("{:?}", receipt.status),
                        "gasUsed": receipt.gas_used.to_string(),
                        "proof": {
                            "type": "receipt_inclusion",
                            "txHash": format!("0x{}", hex::encode(receipt.tx_hash)),
                        },
                    })),
                    None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
                }
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightVerifyShieldedTx
    module
        .register_async_method(
            "call_lightVerifyShieldedTx",
            |params, state, _ctx| async move {
                let call_obj: serde_json::Value =
                    params.one().map_err(|e| invalid_params(e.to_string()))?;
                let nullifiers: Vec<String> = call_obj
                    .get("nullifiers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let commitments: Vec<String> = call_obj
                    .get("commitments")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let mut spent = Vec::new();
                for nf_hex in &nullifiers {
                    if let Ok(bytes) = hex::decode(nf_hex.trim_start_matches("0x")) {
                        let nf =
                            call_shielded::Nullifier(call_primitives::Hash::from_slice(&bytes));
                        if state_accessors::read_shielded_nullifier_spent(&provider, &nf) {
                            spent.push(nf_hex.clone());
                        }
                    }
                }
                let merkle_root = state_accessors::read_shielded_merkle_root(&provider);
                let leaf_count = state_accessors::read_shielded_commitment_count(&provider);
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "valid": spent.is_empty(),
                    "nullifierCount": nullifiers.len(),
                    "commitmentCount": commitments.len(),
                    "alreadySpent": spent,
                    "merkleRoot": format!("{:?}", merkle_root),
                    "leafCount": leaf_count,
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetShieldedBalance
    module
        .register_async_method(
            "call_lightGetShieldedBalance",
            |params, state, _ctx| async move {
                let call_obj: serde_json::Value =
                    params.one().map_err(|e| invalid_params(e.to_string()))?;
                let viewing_key_hex = call_obj
                    .get("viewingKey")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'viewingKey' field"))?;
                let _vk_bytes = hex::decode(viewing_key_hex.trim_start_matches("0x"))
                    .map_err(|e| invalid_params(format!("invalid viewing key: {e}")))?;
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let leaf_count = state_accessors::read_shielded_commitment_count(&provider);
                let merkle_root = state_accessors::read_shielded_merkle_root(&provider);
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "noteCount": leaf_count,
                    "spentNullifiers": 0, // not countable from EVM without iteration
                    "merkleRoot": format!("{:?}", merkle_root),
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Governance Read Endpoints ──────────────────────────────────

    // call_governanceGetProposal
    module
        .register_async_method(
            "call_governanceGetProposal",
            |params, state, _ctx| async move {
                let proposal_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let count = state_accessors::read_gov_proposal_count(&provider);
                if proposal_id == 0 || proposal_id > count {
                    return Ok::<_, ErrorObjectOwned>(serde_json::json!(null));
                }
                let status = state_accessors::read_gov_proposal_status(&provider, proposal_id);
                let proposer = state_accessors::read_gov_proposal_proposer(&provider, proposal_id);
                let title_bytes = state_accessors::read_gov_proposal_title(&provider, proposal_id);
                let desc_bytes =
                    state_accessors::read_gov_proposal_description(&provider, proposal_id);
                let (yes, no, abstain) =
                    state_accessors::read_gov_proposal_votes(&provider, proposal_id);
                let deposit = state_accessors::read_gov_proposal_deposit(&provider, proposal_id);
                let queued_at =
                    state_accessors::read_gov_proposal_queued_at(&provider, proposal_id);
                let title = String::from_utf8_lossy(&title_bytes)
                    .trim_end_matches(|c| c == '\0' || c == '_')
                    .to_string();
                let description = String::from_utf8_lossy(&desc_bytes)
                    .trim_end_matches(|c| c == '\0' || c == '_')
                    .to_string();
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "id": proposal_id,
                    "proposer": format!("{:?}", proposer),
                    "title": title,
                    "description": description,
                    "status": status,
                    "votingPowerYes": yes.to_string(),
                    "votingPowerNo": no.to_string(),
                    "votingPowerAbstain": abstain.to_string(),
                    "deposit": deposit.to_string(),
                    "queuedAt": queued_at,
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceGetAllProposals
    module
        .register_async_method(
            "call_governanceGetAllProposals",
            |_params, state, _ctx| async move {
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let count = state_accessors::read_gov_proposal_count(&provider);
                let mut proposals = Vec::with_capacity(count as usize);
                for id in 1..=count {
                    let status = state_accessors::read_gov_proposal_status(&provider, id);
                    let proposer = state_accessors::read_gov_proposal_proposer(&provider, id);
                    let title_bytes = state_accessors::read_gov_proposal_title(&provider, id);
                    let (yes, no, _abstain) =
                        state_accessors::read_gov_proposal_votes(&provider, id);
                    let title = String::from_utf8_lossy(&title_bytes)
                        .trim_end_matches(|c| c == '\0' || c == '_')
                        .to_string();
                    proposals.push(serde_json::json!({
                        "id": id,
                        "proposer": format!("{:?}", proposer),
                        "title": title,
                        "status": status,
                        "votingPowerYes": yes.to_string(),
                        "votingPowerNo": no.to_string(),
                    }));
                }
                Ok::<_, ErrorObjectOwned>(serde_json::json!({ "proposals": proposals }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceIsPaused
    module
        .register_async_method(
            "call_governanceIsPaused",
            |_params, state, _ctx| async move {
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let paused = state_accessors::read_gov_paused(&provider);
                Ok::<_, ErrorObjectOwned>(serde_json::json!({ "isPaused": paused }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Oracle Read Endpoints ──────────────────────────────────────

    // call_oracleGetPrice
    module
        .register_async_method("call_oracleGetPrice", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                .map_err(|e| db_error(format!("db error: {e}")))?;
            let price =
                call_consensus::exec::state_accessors::read_oracle_price(&provider, asset_id);
            if price == 0 {
                return Ok::<_, ErrorObjectOwned>(serde_json::json!(null));
            }
            let timestamp =
                call_consensus::exec::state_accessors::read_oracle_timestamp(&provider, asset_id);
            let block_number =
                call_consensus::exec::state_accessors::read_oracle_block(&provider, asset_id);
            let count =
                call_consensus::exec::state_accessors::read_oracle_count(&provider, asset_id);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let is_stale = now.saturating_sub(timestamp) > 3600; // 1 hour staleness
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "quoteAssetId": 0,
                "medianPrice": price.to_string(),
                "blockNumber": block_number,
                "timestamp": timestamp,
                "submissionCount": count,
                "outlierCount": 0,
                "isStale": is_stale,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetTwap
    module
        .register_async_method("call_oracleGetTwap", |params, state, _ctx| async move {
            let (asset_id,): (u64,) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                .map_err(|e| db_error(format!("db error: {e}")))?;
            let twap = call_consensus::exec::state_accessors::read_oracle_twap(&provider, asset_id);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "twap": twap.to_string(),
                "windowSecs": 86_400,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetValidatorInfo
    module
        .register_async_method(
            "call_oracleGetValidatorInfo",
            |params, state, _ctx| async move {
                let validator_id: u32 = params.one().map_err(|e| invalid_params(e.to_string()))?;
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let addr = call_consensus::exec::state_accessors::read_validator_addr(
                    &provider,
                    validator_id as u64,
                );
                if addr == call_primitives::Address::ZERO {
                    return Ok::<_, ErrorObjectOwned>(serde_json::json!(null));
                }
                let stake =
                    call_consensus::exec::state_accessors::read_validator_stake(&provider, addr);
                let status =
                    call_consensus::exec::state_accessors::read_validator_status(&provider, addr);
                Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "validatorId": validator_id,
                    "address": format!("{:?}", addr),
                    "stake": stake.to_string(),
                    "isActive": status != 0,
                    "outlierCount": 0,
                    "lastSubmissionBlock": 0,
                    "submissionCount": 0,
                }))
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Bridge Read Endpoints ──────────────────────────────────────

    // call_bridgeGetDepositStatus
    module
        .register_async_method(
            "call_bridgeGetDepositStatus",
            |params, state, _ctx| async move {
                let source_tx_hash_str: String =
                    params.one().map_err(|e| invalid_params(e.to_string()))?;
                let tx_hash_clean = source_tx_hash_str.trim_start_matches("0x");
                let tx_hash_bytes = hex::decode(tx_hash_clean)
                    .map_err(|e| invalid_params(format!("invalid sourceTxHash: {e}")))?;
                if tx_hash_bytes.len() != 32 {
                    return Err(invalid_params("sourceTxHash must be 32 bytes"));
                }
                let mut tx_hash_arr = [0u8; 32];
                tx_hash_arr.copy_from_slice(&tx_hash_bytes);
                let _tx_hash = alloy_primitives::B256::from(tx_hash_arr);
                let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                    .map_err(|e| db_error(format!("db error: {e}")))?;
                let pending_status =
                    state_accessors::read_bridge_pending_status(&provider, tx_hash_arr);
                let is_processed = state_accessors::read_bridge_processed(&provider, tx_hash_arr);
                let status = if pending_status == 1 {
                    let recipient =
                        state_accessors::read_bridge_pending_recipient(&provider, tx_hash_arr);
                    let asset_id =
                        state_accessors::read_bridge_pending_asset(&provider, tx_hash_arr);
                    let amount =
                        state_accessors::read_bridge_pending_amount(&provider, tx_hash_arr);
                    let submitted_at_block =
                        state_accessors::read_bridge_pending_block(&provider, tx_hash_arr);
                    serde_json::json!({
                        "status": "pending",
                        "stage": "challenge_period",
                        "sourceTxHash": source_tx_hash_str,
                        "recipient": format!("{:?}", recipient),
                        "assetId": asset_id,
                        "amount": amount.to_string(),
                        "submittedAtBlock": submitted_at_block,
                    })
                } else if is_processed {
                    serde_json::json!({
                        "status": "finalized",
                        "stage": "complete",
                        "sourceTxHash": source_tx_hash_str,
                    })
                } else {
                    serde_json::json!({
                        "status": "not_found",
                        "sourceTxHash": source_tx_hash_str,
                    })
                };
                Ok::<_, ErrorObjectOwned>(status)
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Light Client Bridge (Blocked — Direct Execution Removed) ──

    // call_lightClientBridgeDeposit — BLOCKED.
    // Previously performed direct EVM state writes from the RPC layer, which
    // violates the EVM-Only State Architecture (state mutations must go through
    // consensus/EVM execution). Disabled until re-implemented as a precompile
    // transaction path (see docs/light-client.md).
    module
        .register_async_method("call_lightClientBridgeDeposit", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "call_lightClientBridgeDeposit is disabled: direct EVM writes are not permitted. Use standard bridge deposit flow.",
                None::<()>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Validator Read Endpoints ───────────────────────────────────

    // call_validatorList
    module
        .register_async_method("call_validatorList", |_params, state, _ctx| async move {
            let provider = call_evm::provider::InMemoryStateProvider::from_db(&state.db_env)
                .map_err(|e| db_error(format!("db error: {e}")))?;
            let count = state_accessors::read_validator_count(&provider);
            let mut validators = Vec::new();
            for i in 1..=count {
                let addr = state_accessors::read_validator_addr(&provider, i);
                if addr == Address::ZERO {
                    continue;
                }
                let status = state_accessors::read_validator_status(&provider, addr);
                if status == 0 {
                    continue;
                }
                validators.push(serde_json::json!({
                    "validatorId": i,
                    "address": format!("0x{}", hex::encode(addr.as_slice())),
                    "ed25519Pubkey": format!("0x{}", hex::encode(state_accessors::read_validator_pubkey(&provider, addr))),
                    "selfStake": state_accessors::read_validator_stake(&provider, addr).to_string(),
                    "stakedCall": state_accessors::read_validator_stake(&provider, addr).to_string(),
                    "delegatedCall": "0",
                    "rewards": "0",
                    "isUnbonding": status == 2,
                }));
            }
            Ok::<_, ErrorObjectOwned>(serde_json::json!({ "validators": validators }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    Ok(())
}

//! Callchain extension RPC endpoints (per spec §11.2)
//!
//! Architecture: all state-mutating operations go through EVM precompiles.
//! Read-only endpoints remain as individual `call_*` methods.

use crate::handlers::state::RpcState;
use crate::handlers::helpers::{invalid_params, internal_error};
use call_consensus::exec::evm_instructions;
use call_primitives::Address;
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::Arc;

/// Register Callchain extension RPC methods
pub fn register_callchain_rpc(module: &mut RpcModule<Arc<RpcState>>) -> Result<(), ErrorObjectOwned> {
    // ── Unified Write Endpoint ─────────────────────────────────────

    // call_submit — DEPRECATED: The mempool is EVM-only.
    // Submit transactions via eth_sendRawTransaction instead.
    module
        .register_async_method("call_submit", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "call_submit is deprecated: submit via eth_sendRawTransaction",
                None::<()>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Read-Only Query Endpoints ──────────────────────────────────

    // call_assetInfo
    module
        .register_async_method("call_assetInfo", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            match state.get_asset_info(asset_id) {
                Some(info) => Ok::<_, ErrorObjectOwned>(serde_json::to_value(info).map_err(|e| internal_error(e.to_string()))?),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_protocolBalance
    module
        .register_async_method("call_protocolBalance", |params, state, _ctx| async move {
            let (asset_id, address): (u64, String) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
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
            let addr = address.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
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
                Some(info) => Ok::<_, ErrorObjectOwned>(serde_json::to_value(info).map_err(|e| internal_error(e.to_string()))?),
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
                        .map(|r| serde_json::json!({
                            "txHash": format!("0x{}", hex::encode(r.tx_hash)),
                            "status": format!("{:?}", r.status),
                            "gasUsed": r.gas_used.to_string(),
                            "feeAmount": r.fee_amount.to_string(),
                        }))
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
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "shielded deposit proving requires a local prover — use `call-cli shielded deposit-prove <args>` or run a dedicated proving service with `--prover-mode deposit`",
                None::<()>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedTransferProve
    module
        .register_async_method("call_shieldedTransferProve", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "shielded transfer proving requires a local prover — use `call-cli shielded transfer-prove <args>` or run a dedicated proving service with `--prover-mode transfer`",
                None::<()>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedBalance
    module
        .register_async_method("call_shieldedBalance", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let vk_hex = call_obj.get("viewingKey")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'viewingKey' field".into()))?;
            let vk_bytes = hex::decode(vk_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid viewing key: {e}")))?;
            if vk_bytes.len() < 32 {
                return Err(invalid_params("viewing key must be at least 32 bytes".into()));
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
            let evm = state.evm_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let merkle_root = evm_instructions::read_shielded_merkle_root(&evm);
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
        .register_async_method("call_shieldedTreeState", |_params, state, _ctx| async move {
            let tree_state = state.get_shielded_tree_state();
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "merkleRoot": format!("{:?}", tree_state.merkle_root),
                "leafCount": tree_state.leaf_count,
                "nullifierCount": tree_state.nullifier_count,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Receipt / Log Read Endpoints ───────────────────────────────

    // call_getTransactionReceipt
    module
        .register_async_method("call_getTransactionReceipt", |params, state, _ctx| async move {
            let tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let hash = tx_hash.parse::<alloy_primitives::B256>().map_err(|e| invalid_params(e.to_string()))?;
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
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_getBlockReceipts
    module
        .register_async_method("call_getBlockReceipts", |params, state, _ctx| async move {
            let block: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let receipts = state.get_receipts_by_block(block);
            let result: Vec<serde_json::Value> = receipts
                .iter()
                .map(|r| serde_json::json!({
                    "txHash": format!("{:?}", r.tx_hash),
                    "status": format!("{:?}", r.status),
                    "gasUsed": r.gas_used.to_string(),
                }))
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
            let hash = ref_str.parse::<alloy_primitives::B256>()
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
            let fm = state.fork_manager.read().map_err(|_| internal_error("lock poisoned".into()))?;
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
            let fm = state.fork_manager.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let current_block = *state.current_block.read().map_err(|_| internal_error("lock poisoned".into()))?;
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
        .register_async_method("call_lightVerifyBlockHeader", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let header_json = call_obj.get("header")
                .ok_or_else(|| invalid_params("missing 'header' field".into()))?;
            let sigs_json = call_obj.get("signatures")
                .ok_or_else(|| invalid_params("missing 'signatures' field".into()))?;
            let header: call_consensus::BlockHeader = serde_json::from_value(header_json.clone())
                .map_err(|e| invalid_params(format!("invalid header: {e}")))?;
            let block_hash = header.hash();
            let sigs_array = sigs_json.get("signatures")
                .and_then(|v| v.as_array())
                .ok_or_else(|| invalid_params("missing signatures array".into()))?;
            let evm = state.evm_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let validators = evm_instructions::read_validator_addresses(&*evm);
            let total = validators.len() as u32;
            let quorum = (2 * total as usize).div_ceil(3).max(1);
            let block_hash_bytes = block_hash.as_slice();
            let mut valid_count = 0;
            for entry in sigs_array {
                if let Some(validator_id) = entry.get(0).and_then(|v| v.as_u64()) {
                    let addr = evm_instructions::read_validator_addr(&*evm, validator_id);
                    if addr != Address::ZERO {
                        let status = evm_instructions::read_validator_status(&*evm, addr);
                        if status != 0 {
                            if let Some(sig_hex) = entry.get(2).and_then(|v| v.as_str()) {
                                if let Ok(sig_bytes) = hex::decode(sig_hex.trim_start_matches("0x")) {
                                    if sig_bytes.len() == 64 {
                                        let mut sig = [0u8; 64];
                                        sig.copy_from_slice(&sig_bytes);
                                        let pk = evm_instructions::read_validator_pubkey(&*evm, addr);
                                        if call_crypto::ed25519_verify(
                                            &pk,
                                            &sig,
                                            block_hash_bytes,
                                        ).is_ok() {
                                            valid_count += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let valid = valid_count >= quorum && header.timestamp_millis > 0 && header.proposer > 0;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "valid": valid,
                "height": header.height,
                "hash": format!("{block_hash:?}"),
                "signatureCount": valid_count,
                "quorum": quorum,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetBalanceProof
    module
        .register_async_method("call_lightGetBalanceProof", |params, state, _ctx| async move {
            let (asset_id, address_str): (u64, String) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let address = address_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
            let balance = state.get_balance(asset_id, &address);
            let evm = state.evm_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let merkle_root = evm_instructions::read_shielded_merkle_root(&evm);
            let leaf_count = evm_instructions::read_shielded_commitment_count(&evm);
            // Full note proofs require the Merkle tree structure (not in EVM storage).
            let state_leaf = call_crypto::keccak256(format!("{asset_id}:{address:?}:{balance}").as_bytes());
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
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetTransactionProof
    module
        .register_async_method("call_lightGetTransactionProof", |params, state, _ctx| async move {
            let tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let hash = tx_hash.parse::<alloy_primitives::B256>()
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
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightVerifyShieldedTx
    module
        .register_async_method("call_lightVerifyShieldedTx", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let nullifiers: Vec<String> = call_obj.get("nullifiers")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let commitments: Vec<String> = call_obj.get("commitments")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let evm = state.evm_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let mut spent = Vec::new();
            for nf_hex in &nullifiers {
                if let Ok(bytes) = hex::decode(nf_hex.trim_start_matches("0x")) {
                    let nf = call_shielded::Nullifier(call_primitives::Hash::from_slice(&bytes));
                    if evm_instructions::read_shielded_nullifier_spent(&evm, &nf) {
                        spent.push(nf_hex.clone());
                    }
                }
            }
            let merkle_root = evm_instructions::read_shielded_merkle_root(&evm);
            let leaf_count = evm_instructions::read_shielded_commitment_count(&evm);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "valid": spent.is_empty(),
                "nullifierCount": nullifiers.len(),
                "commitmentCount": commitments.len(),
                "alreadySpent": spent,
                "merkleRoot": format!("{:?}", merkle_root),
                "leafCount": leaf_count,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_lightGetShieldedBalance
    module
        .register_async_method("call_lightGetShieldedBalance", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let viewing_key_hex = call_obj.get("viewingKey")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'viewingKey' field".into()))?;
            let _vk_bytes = hex::decode(viewing_key_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid viewing key: {e}")))?;
            let evm = state.evm_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let leaf_count = evm_instructions::read_shielded_commitment_count(&evm);
            let merkle_root = evm_instructions::read_shielded_merkle_root(&evm);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "noteCount": leaf_count,
                "spentNullifiers": 0, // not countable from EVM without iteration
                "merkleRoot": format!("{:?}", merkle_root),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Governance Read Endpoints ──────────────────────────────────

    // call_governanceGetProposal
    module
        .register_async_method("call_governanceGetProposal", |params, state, _ctx| async move {
            let proposal_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let gov = state.governance.read().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.get_proposal(proposal_id) {
                Some(p) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "id": p.id,
                    "proposer": format!("{:?}", p.proposer),
                    "title": p.title,
                    "description": p.description,
                    "state": format!("{:?}", p.state),
                    "votingPowerYes": p.voting_power_yes.to_string(),
                    "votingPowerNo": p.voting_power_no.to_string(),
                    "votingPowerAbstain": p.voting_power_abstain.to_string(),
                    "quorumRequired": p.quorum_required.to_string(),
                    "startBlock": p.start_block,
                    "endBlock": p.end_block,
                    "executionBlock": p.execution_block,
                })),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceGetAllProposals
    module
        .register_async_method("call_governanceGetAllProposals", |_params, state, _ctx| async move {
            let gov = state.governance.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let proposals: Vec<serde_json::Value> = gov.get_all_proposals().values()
                .map(|p| serde_json::json!({
                    "id": p.id,
                    "proposer": format!("{:?}", p.proposer),
                    "title": p.title,
                    "state": format!("{:?}", p.state),
                    "votingPowerYes": p.voting_power_yes.to_string(),
                    "votingPowerNo": p.voting_power_no.to_string(),
                }))
                .collect();
            Ok::<_, ErrorObjectOwned>(serde_json::json!({ "proposals": proposals }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceIsPaused
    module
        .register_async_method("call_governanceIsPaused", |_params, state, _ctx| async move {
            let gov = state.governance.read().map_err(|_| internal_error("lock poisoned".into()))?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({ "isPaused": gov.is_paused() }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Oracle Read Endpoints ──────────────────────────────────────

    // call_oracleGetPrice
    module
        .register_async_method("call_oracleGetPrice", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let price = call_consensus::exec::evm_instructions::read_oracle_price(&evm, asset_id);
            if price == 0 {
                return Ok::<_, ErrorObjectOwned>(serde_json::json!(null));
            }
            let timestamp = call_consensus::exec::evm_instructions::read_oracle_timestamp(&evm, asset_id);
            let block_number = call_consensus::exec::evm_instructions::read_oracle_block(&evm, asset_id);
            let count = call_consensus::exec::evm_instructions::read_oracle_count(&evm, asset_id);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
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
            let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let twap = call_consensus::exec::evm_instructions::read_oracle_twap(&evm, asset_id);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "twap": twap.to_string(),
                "windowSecs": 86_400,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetValidatorInfo
    module
        .register_async_method("call_oracleGetValidatorInfo", |params, state, _ctx| async move {
            let validator_id: u32 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let addr = call_consensus::exec::evm_instructions::read_validator_addr(
                &evm, validator_id as u64);
            if addr == call_primitives::Address::ZERO {
                return Ok::<_, ErrorObjectOwned>(serde_json::json!(null));
            }
            let stake = call_consensus::exec::evm_instructions::read_validator_stake(&evm, addr);
            let status = call_consensus::exec::evm_instructions::read_validator_status(&evm, addr);
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "validatorId": validator_id,
                "address": format!("{:?}", addr),
                "stake": stake.to_string(),
                "isActive": status != 0,
                "outlierCount": 0,
                "lastSubmissionBlock": 0,
                "submissionCount": 0,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Bridge Read Endpoints ──────────────────────────────────────

    // call_bridgeGetDepositStatus
    module
        .register_async_method("call_bridgeGetDepositStatus", |params, state, _ctx| async move {
            let source_tx_hash_str: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let tx_hash_clean = source_tx_hash_str.trim_start_matches("0x");
            let tx_hash_bytes = hex::decode(tx_hash_clean)
                .map_err(|e| invalid_params(format!("invalid sourceTxHash: {e}")))?;
            if tx_hash_bytes.len() != 32 {
                return Err(invalid_params("sourceTxHash must be 32 bytes".into()));
            }
            let mut tx_hash_arr = [0u8; 32];
            tx_hash_arr.copy_from_slice(&tx_hash_bytes);
            let tx_hash = alloy_primitives::B256::from(tx_hash_arr);
            let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let pending_status = evm_instructions::read_bridge_pending_status(&*evm, tx_hash_arr);
            let is_processed = evm_instructions::read_bridge_processed(&*evm, tx_hash_arr);
            let status = if pending_status == 1 {
                let recipient = evm_instructions::read_bridge_pending_recipient(&*evm, tx_hash_arr);
                let asset_id = evm_instructions::read_bridge_pending_asset(&*evm, tx_hash_arr);
                let amount = evm_instructions::read_bridge_pending_amount(&*evm, tx_hash_arr);
                let submitted_at_block = evm_instructions::read_bridge_pending_block(&*evm, tx_hash_arr);
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
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Light Client Bridge (Direct Execution — not yet instruction-based)

    // call_lightClientBridgeDeposit
    module
        .register_async_method("call_lightClientBridgeDeposit", |_params, _state, _ctx| async move {
            #[cfg(not(feature = "light-client-bridge"))]
            return Err::<serde_json::Value, _>(internal_error(
                "light client bridge is not enabled".into()));

            #[cfg(feature = "light-client-bridge")]
            async {
                let call_obj: serde_json::Value = _params.one().map_err(|e| invalid_params(e.to_string()))?;
                let header_hex = call_obj.get("headerRlp")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'headerRlp' field".into()))?;
                let header_bytes = hex::decode(header_hex.trim_start_matches("0x"))
                    .map_err(|e| invalid_params(format!("invalid headerRlp: {e}")))?;
                let source_chain_str = call_obj.get("sourceChain")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'sourceChain' field".into()))?;
                let source_chain = match source_chain_str.to_lowercase().as_str() {
                    "ethereum" | "ethereummainnet" => call_bridge::ExternalChain::EthereumMainnet,
                    "arbitrum" => call_bridge::ExternalChain::Arbitrum,
                    _ => return Err(invalid_params("unknown source chain".into())),
                };
                let recipient_hex = call_obj.get("recipient")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'recipient' field".into()))?;
                let recipient = recipient_hex.parse::<alloy_primitives::Address>()
                    .map_err(|e| invalid_params(format!("invalid recipient: {e}")))?;
                let asset_id = call_obj.get("assetId")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| invalid_params("missing 'assetId' field".into()))?;
                let amount_str = call_obj.get("amount")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'amount' field (must be string)".into()))?;
                let amount: u128 = amount_str
                    .parse()
                    .map_err(|_| invalid_params("invalid amount: must be a numeric string".into()))?;
                let tx_proof_nodes: Vec<Vec<u8>> = call_obj.get("txProof")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())).collect())
                    .ok_or_else(|| invalid_params("missing 'txProof' field".into()))?;
                let receipt_proof_nodes: Vec<Vec<u8>> = call_obj.get("receiptProof")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())).collect())
                    .ok_or_else(|| invalid_params("missing 'receiptProof' field".into()))?;
                let receipt_index = call_obj.get("receiptIndex")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| invalid_params("missing 'receiptIndex' field".into()))?;
                use call_light_client::{EthHeader, TxInclusionProof, ReceiptProof, MptProofNode};
                let header = EthHeader::from_rlp(header_bytes);
                let tx_proof = TxInclusionProof::new(
                    tx_proof_nodes.into_iter().map(MptProofNode::new).collect(),
                );
                let receipt_proof = ReceiptProof::new(
                    receipt_index,
                    receipt_proof_nodes.into_iter().map(MptProofNode::new).collect(),
                );
                let op = call_bridge::ExternalBridgeOp::LightClientDeposit {
                    source_chain,
                    header,
                    tx_proof,
                    receipt_proof,
                    recipient,
                    asset_id,
                    amount,
                };
                let mut light_client_guard = _state.light_client.write().map_err(|_| internal_error("lock poisoned".into()))?;
                let light_client = light_client_guard.as_mut()
                    .ok_or_else(|| invalid_params("light client not initialized".into()))?;
                let config = call_bridge::BridgeConfig::default();
                let current_block = _state.get_current_block();
                let mut evm_state = _state.evm_state.write().map_err(|_| internal_error("lock poisoned".into()))?;
                match call_bridge::process_light_client_deposit_evm(light_client, &op, &mut evm_state, &config, current_block) {
                    Ok(call_bridge::ExternalDepositResult::Queued { finalized_at_block, .. }) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                        "status": "queued",
                        "assetId": asset_id,
                        "amount": amount.to_string(),
                        "recipient": format!("0x{}", hex::encode(recipient.as_slice())),
                        "challengePeriodBlocks": config.challenge_period_blocks,
                        "finalizedAtBlock": finalized_at_block,
                    })),
                    Err(e) => Err(invalid_params(e.to_string())),
                }
            }.await
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Validator Read Endpoints ───────────────────────────────────

    // call_validatorList
    module
        .register_async_method("call_validatorList", |_params, state, _ctx| async move {
            let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let count = evm_instructions::read_validator_count(&*evm);
            let mut validators = Vec::new();
            for i in 1..=count {
                let addr = evm_instructions::read_validator_addr(&*evm, i);
                if addr == Address::ZERO {
                    continue;
                }
                let status = evm_instructions::read_validator_status(&*evm, addr);
                if status == 0 {
                    continue;
                }
                validators.push(serde_json::json!({
                    "validatorId": i,
                    "address": format!("0x{}", hex::encode(addr.as_slice())),
                    "ed25519Pubkey": format!("0x{}", hex::encode(evm_instructions::read_validator_pubkey(&*evm, addr))),
                    "selfStake": evm_instructions::read_validator_stake(&*evm, addr).to_string(),
                    "stakedCall": evm_instructions::read_validator_stake(&*evm, addr).to_string(),
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

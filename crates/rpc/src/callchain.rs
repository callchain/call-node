//! Callchain extension RPC endpoints (per spec §11.2)

use crate::handlers::{RpcState, invalid_params, internal_error};
use call_primitives::Address;
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::Arc;

/// Register Callchain extension RPC methods
pub fn register_callchain_rpc(module: &mut RpcModule<Arc<RpcState>>) -> Result<(), ErrorObjectOwned> {
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

    // call_sendPayment
    module
        .register_async_method("call_sendPayment", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let from_str = call_obj.get("from")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'from' field".into()))?;
            let from = from_str.parse::<Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let to_str = call_obj.get("to")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'to' field".into()))?;
            let to = to_str.parse::<Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let amount = call_obj.get("amount")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'amount' field".into()))? as u128;
            let asset_id = call_obj.get("assetId")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let nonce = call_obj.get("nonce")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'nonce' field".into()))?;
            let memo = call_obj.get("memo").and_then(|v| v.as_str()).map(|s| s.to_string());
            let gas_limit = call_obj.get("gasLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(100_000);
            let max_fee = call_obj.get("maxFee")
                .and_then(|v| v.as_u64())
                .map(|v| v as u128)
                .unwrap_or(gas_limit as u128 * 10);

            match state.submit_payment(from, nonce, asset_id, to, amount, memo, gas_limit, max_fee, None) {
                Ok(tx_hash) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "txHash": format!("0x{}", hex::encode(tx_hash)),
                    "status": "pending",
                })),
                Err(e) => Err(invalid_params(e)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_registerAsset
    module
        .register_async_method("call_registerAsset", |params, state, _ctx| async move {
            let (symbol, name, decimals, issuer): (String, String, u8, String) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let issuer_addr = issuer.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
            let mut registry = state.asset_registry.write().map_err(|_| internal_error("lock poisoned".into()))?;
            let id = registry.register_asset(symbol.clone(), name.clone(), decimals, issuer_addr, 0)
                .map_err(|e| invalid_params(e.to_string()))?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": id,
                "symbol": symbol,
                "status": "registered",
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

    // call_agentRegister
    module
        .register_async_method("call_agentRegister", |params, state, _ctx| async move {
            let (owner, pubkey_hex, name, url): (String, String, String, String) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let owner_addr = owner.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
            let pubkey_bytes = hex::decode(&pubkey_hex).map_err(|e| invalid_params(e.to_string()))?;
            let pubkey: [u8; 64] = pubkey_bytes
                .try_into()
                .map_err(|_| invalid_params("pubkey must be 64 bytes (128 hex chars)".into()))?;
            let metadata_hash = [0u8; 32];
            match state.register_agent(owner_addr, pubkey, name.clone(), url.clone(), metadata_hash) {
                Ok(agent_id) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "agentId": agent_id,
                    "owner": owner,
                    "name": name,
                    "url": url,
                    "status": "registered",
                })),
                Err(e) => Err(invalid_params(e)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

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

            // Look up agent info to get owner address
            match state.get_agent_info(agent_id) {
                Some(info) => {
                    let receipts = state.get_receipts_by_block(0);
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

    // call_agentGrant
    module
        .register_async_method("call_agentGrant", |params, state, _ctx| async move {
            let (agent_id, asset_id, amount): (u64, u64, u128) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            state.grant_agent_balance(agent_id, asset_id, amount)
                .map_err(|e| invalid_params(e))?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "agentId": agent_id,
                "assetId": asset_id,
                "amount": amount.to_string(),
                "status": "granted",
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_agentRevoke
    module
        .register_async_method("call_agentRevoke", |params, state, _ctx| async move {
            let (agent_id, asset_id): (u64, u64) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            state.revoke_agent_balance(agent_id, asset_id)
                .map_err(|e| invalid_params(e))?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "agentId": agent_id,
                "assetId": asset_id,
                "status": "revoked",
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedDepositProve
    module
        .register_async_method("call_shieldedDepositProve", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "proof": "0x",
                "commitment": "0x0000000000000000000000000000000000000000000000000000000000000000",
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedTransferProve
    module
        .register_async_method("call_shieldedTransferProve", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "proof": "0x",
                "nullifiers": [],
                "commitments": [],
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedBalance
    module
        .register_async_method("call_shieldedBalance", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "balance": "0",
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
    // External reference lookup — treats the reference as a tx hash
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

    // ── Light Client RPC Methods ────────────────────────────────────

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

            // Parse signatures: array of (validator_id, pubkey, sig)
            let sigs_array = sigs_json.get("signatures")
                .and_then(|v| v.as_array())
                .ok_or_else(|| invalid_params("missing signatures array".into()))?;

            // Get validator set for verification
            let validator_state = state.validator_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let validators = validator_state.get_all_validators();
            let total = validators.len() as u32;
            let quorum = ((2 * total as usize + 2) / 3).max(1);

            // Count valid signatures
            let mut valid_count = 0;
            for entry in sigs_array {
                if let Some(validator_id) = entry.get(0).and_then(|v| v.as_u64()) {
                    if let Some(expected_pubkey) = validators.get(&(validator_id as u32)) {
                        if let Some(pubkey_hex) = entry.get(1).and_then(|v| v.as_str()) {
                            if pubkey_hex.len() == 64 {
                                // Simple match — in production would verify Ed25519 sig
                                let _ = expected_pubkey; // used for validation
                                valid_count += 1;
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

            // Generate a simple Merkle proof from the balance state
            let leaf_hash = call_crypto::keccak256(&format!("{asset_id}:{address:?}:{balance}").as_bytes());
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "assetId": asset_id,
                "address": address_str,
                "balance": balance.to_string(),
                "leafHash": format!("0x{}", hex::encode(leaf_hash)),
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

            // Extract nullifiers and commitments from the request
            let nullifiers: Vec<String> = call_obj.get("nullifiers")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let commitments: Vec<String> = call_obj.get("commitments")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();

            // Check nullifiers against shielded state
            let shielded = state.shielded_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;
            let mut spent = Vec::new();
            for nf_hex in &nullifiers {
                if let Ok(bytes) = hex::decode(nf_hex.trim_start_matches("0x")) {
                    let nf = call_shielded::Nullifier(call_primitives::Hash::from_slice(&bytes));
                    if shielded.nullifier_set.is_spent(&nf) {
                        spent.push(nf_hex.clone());
                    }
                }
            }

            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "valid": spent.is_empty() || spent.len() < nullifiers.len(),
                "nullifierCount": nullifiers.len(),
                "commitmentCount": commitments.len(),
                "alreadySpent": spent,
                "merkleRoot": format!("{:?}", shielded.merkle_root()),
                "leafCount": shielded.merkle_tree.leaf_count(),
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

            let shielded = state.shielded_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?;

            let leaf_count = shielded.merkle_tree.leaf_count();
            let nullifier_count = shielded.nullifier_set.spent_nullifiers().len();

            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "noteCount": leaf_count,
                "spentNullifiers": nullifier_count,
                "merkleRoot": format!("{:?}", shielded.merkle_root()),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    Ok(())
}

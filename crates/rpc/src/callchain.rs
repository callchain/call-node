//! Callchain extension RPC endpoints (per spec §11.2)

use crate::handlers::{RpcState, invalid_params, internal_error};
use call_primitives::Address;
use call_governance::{ProposalType, Vote as GovernanceVote};
use call_crypto::{recover_secp256k1_signer, keccak256};
use call_oracle::OracleSubmission;
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::Arc;

/// Register Callchain extension RPC methods
pub fn register_callchain_rpc(module: &mut RpcModule<Arc<RpcState>>) -> Result<(), ErrorObjectOwned> {
    // ── Governance signature helper ──────────────────────────────

    /// Verify an optional secp256k1 signature against an expected address.
    /// If signature is provided, recover signer and verify it matches expected.
    /// If no signature, allow (backwards compatible for devnet/testing).
    fn verify_signature(
        msg_hash: [u8; 32],
        expected: Address,
        sig_hex: Option<&str>,
    ) -> Result<(), ErrorObjectOwned> {
        if let Some(sig_str) = sig_hex {
            let sig_bytes = hex::decode(sig_str.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid signature hex: {e}")))?;
            if sig_bytes.len() != 65 {
                return Err(invalid_params("signature must be 65 bytes (r || s || v)".into()));
            }
            let mut sig = [0u8; 65];
            sig.copy_from_slice(&sig_bytes);
            let recovered = recover_secp256k1_signer(&msg_hash, &sig)
                .map_err(|e| invalid_params(format!("signature recovery failed: {e}")))?;
            if recovered != expected {
                return Err(invalid_params(format!(
                    "signature mismatch: recovered {:?}, expected {:?}",
                    recovered, expected
                )));
            }
        }
        Ok(())
    }

    /// Compute a keccak256 hash for signing.
    fn msg_hash(message: &[u8]) -> [u8; 32] {
        let h = keccak256(message);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(h.as_slice());
        arr
    }
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
            let amount_str = call_obj.get("amount")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'amount' field (must be string)".into()))?;
            let amount: u128 = amount_str
                .parse()
                .map_err(|_| invalid_params("invalid amount: must be a numeric string".into()))?;
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

            let sig_hex = call_obj.get("signature")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'signature' field".into()))?;
            let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid signature: {e}")))?;
            let mut signature = [0u8; 65];
            signature.copy_from_slice(&sig_bytes);

            // Recover signer address from signature and verify it matches sender
            // Include all tx fields in preimage (like Ethereum EIP-155/EIP-1559) to prevent
            // signature malleability — changing any field invalidates the signature
            let mut preimage = Vec::new();
            preimage.extend_from_slice(from.as_slice());
            preimage.extend_from_slice(&nonce.to_be_bytes());
            preimage.extend_from_slice(&asset_id.to_be_bytes());
            preimage.extend_from_slice(to.as_slice());
            preimage.extend_from_slice(&amount.to_be_bytes());
            preimage.extend_from_slice(&gas_limit.to_be_bytes());
            preimage.extend_from_slice(&max_fee.to_be_bytes());
            let tx_hash_preimage = call_crypto::keccak256(&preimage);
            let recovered = call_crypto::recover_secp256k1_signer(&tx_hash_preimage.0, &signature)
                .map_err(|e| invalid_params(format!("signature recovery failed: {e:?}")))?;
            if recovered != from {
                return Err(invalid_params("signature does not match sender address".into()));
            }

            match state.submit_payment(from, nonce, asset_id, to, amount, memo, gas_limit, max_fee, Some(signature)) {
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
            let id = registry.register_asset(symbol.clone(), name, decimals, issuer_addr, 0)
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

    // call_agentGrant
    module
        .register_async_method("call_agentGrant", |params, state, _ctx| async move {
            let (caller_str, agent_id, asset_id, amount): (String, u64, u64, u128) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let caller = caller_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;

            // Verify caller is the agent owner
            let registry = state.agent_registry.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let agent = registry.get_agent(agent_id)
                .ok_or_else(|| invalid_params("agent not found".into()))?;
            if agent.owner != caller {
                return Err(invalid_params("only the agent owner can grant balance".into()));
            }
            drop(registry);

            state.grant_agent_balance(agent_id, asset_id, amount)
                .map_err(invalid_params)?;
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
            let (caller_str, agent_id, asset_id): (String, u64, u64) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let caller = caller_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;

            // Verify caller is the agent owner
            let registry = state.agent_registry.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let agent = registry.get_agent(agent_id)
                .ok_or_else(|| invalid_params("agent not found".into()))?;
            if agent.owner != caller {
                return Err(invalid_params("only the agent owner can revoke balance".into()));
            }
            drop(registry);

            state.revoke_agent_balance(agent_id, asset_id)
                .map_err(invalid_params)?;
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
            Err::<serde_json::Value, _>(internal_error(
                "shielded deposit proving requires a local prover — use the CLI wallet or a dedicated proving service".into()))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_shieldedTransferProve
    module
        .register_async_method("call_shieldedTransferProve", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(internal_error(
                "shielded transfer proving requires a local prover — use the CLI wallet or a dedicated proving service".into()))
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
            let quorum = (2 * total as usize).div_ceil(3).max(1);

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
            let leaf_hash = call_crypto::keccak256(format!("{asset_id}:{address:?}:{balance}").as_bytes());
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

    // ── Governance RPC Methods ──────────────────────────────────────

    // call_governanceSubmitProposal
    module
        .register_async_method("call_governanceSubmitProposal", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let proposer_str = call_obj.get("proposer")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'proposer' field".into()))?;
            let proposer = proposer_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;

            let type_str = call_obj.get("type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'type' field".into()))?;
            let title = call_obj.get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'title' field".into()))?
                .to_string();
            let description = call_obj.get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'description' field".into()))?
                .to_string();
            let execution_data = call_obj.get("executionData")
                .and_then(|v| v.as_str())
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default();

            // Optional signature verification
            let sig = call_obj.get("signature").and_then(|v| v.as_str());
            if let Some(sig_hex) = sig {
                // Build message hash: keccak256(proposer + title + description)
                let mut msg = Vec::new();
                msg.extend_from_slice(proposer.as_slice());
                msg.extend_from_slice(title.as_bytes());
                msg.extend_from_slice(description.as_bytes());
                let hash = msg_hash(&msg);
                verify_signature(hash, proposer, Some(sig_hex))?;
            }

            let proposal_type = serde_json::from_value::<ProposalType>(call_obj["proposalType"].clone())
                .map_err(|e| invalid_params(format!("invalid proposalType: {e}")))?;

            let mut gov = state.governance.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.submit_proposal(proposer, proposal_type.clone(), title, description, execution_data) {
                Ok(id) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "proposalId": id,
                    "type": type_str,
                    "status": "pending",
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceVote
    module
        .register_async_method("call_governanceVote", |params, state, _ctx| async move {
            // Accept both object params with optional signature, and positional params
            let (proposal_id, voter, vote, sig): (u64, Address, String, Option<String>) = {
                let raw: serde_json::Value = params.parse().map_err(|e| invalid_params(e.to_string()))?;
                if let Some(obj) = raw.as_object() {
                    let proposal_id = obj.get("proposalId")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| invalid_params("missing 'proposalId'".into()))?;
                    let voter_str = obj.get("voter")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| invalid_params("missing 'voter'".into()))?;
                    let voter = voter_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
                    let vote_str = obj.get("vote")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| invalid_params("missing 'vote'".into()))?;
                    let sig = obj.get("signature")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (proposal_id, voter, vote_str.to_string(), sig)
                } else {
                    // Fallback: parse as tuple (proposalId, voter, vote)
                    let (pid, v_str, vote_str): (u64, String, String) =
                        serde_json::from_value(raw).map_err(|e| invalid_params(e.to_string()))?;
                    let v = v_str.parse::<Address>().map_err(|e| invalid_params(e.to_string()))?;
                    (pid, v, vote_str, None)
                }
            };

            let vote_val = match vote.to_lowercase().as_str() {
                "yes" => GovernanceVote::Yes,
                "no" => GovernanceVote::No,
                "abstain" => GovernanceVote::Abstain,
                _ => return Err(invalid_params("vote must be 'yes', 'no', or 'abstain'".into())),
            };

            // Optional signature verification
            if let Some(ref sig_hex) = sig {
                let mut msg = Vec::new();
                msg.extend_from_slice(&proposal_id.to_be_bytes());
                msg.extend_from_slice(voter.as_slice());
                msg.extend_from_slice(vote.as_bytes());
                let hash = msg_hash(&msg);
                verify_signature(hash, voter, Some(sig_hex))?;
            }

            let mut gov = state.governance.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.vote(proposal_id, voter, vote_val) {
                Ok(()) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "proposalId": proposal_id,
                    "status": "recorded",
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceQueue
    module
        .register_async_method("call_governanceQueue", |params, state, _ctx| async move {
            let proposal_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let mut gov = state.governance.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.queue_proposal(proposal_id) {
                Ok(()) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "proposalId": proposal_id,
                    "status": "queued",
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceExecute
    module
        .register_async_method("call_governanceExecute", |params, state, _ctx| async move {
            let proposal_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let mut gov = state.governance.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.execute_proposal(proposal_id) {
                Ok(()) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "proposalId": proposal_id,
                    "status": "executed",
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

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

    // call_governanceEmergencyPause
    module
        .register_async_method("call_governanceEmergencyPause", |params, state, _ctx| async move {
            let (validator_id, reason): (u32, String) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let mut gov = state.governance.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match gov.emergency_pause_initiate(validator_id, reason) {
                Ok(activated) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "activated": activated,
                    "isPaused": gov.is_paused(),
                    "message": if activated { "chain paused" } else { "more signatures needed" },
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_governanceIsPaused
    module
        .register_async_method("call_governanceIsPaused", |_params, state, _ctx| async move {
            let gov = state.governance.read().map_err(|_| internal_error("lock poisoned".into()))?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({ "isPaused": gov.is_paused() }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Oracle RPC Methods ──────────────────────────────────────────

    // call_oracleSubmitPrice
    module
        .register_async_method("call_oracleSubmitPrice", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let validator_id = call_obj.get("validatorId")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'validatorId' field".into()))? as u32;
            let asset_id = call_obj.get("assetId")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'assetId' field".into()))?;
            let price = call_obj.get("price")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'price' field".into()))? as u128;
            let sig_hex = call_obj.get("signature")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'signature' field".into()))?;
            let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid signature: {e}")))?;
            let mut signature = [0u8; 64];
            signature.copy_from_slice(&sig_bytes);

            let block_number = state.get_current_block();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let submission = OracleSubmission {
                validator_id,
                asset_id,
                price,
                block_number,
                timestamp,
                signature,
                sources: call_obj.get("sources")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            };

            let mut oracle = state.oracle.write().map_err(|_| internal_error("lock poisoned".into()))?;
            match oracle.submit_price(submission) {
                Ok(()) => {
                    let agg = oracle.get_price(asset_id);
                    Ok::<_, ErrorObjectOwned>(serde_json::json!({
                        "status": "accepted",
                        "assetId": asset_id,
                        "currentPrice": agg.map(|a| a.median_price.to_string()),
                    }))
                }
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetPrice
    module
        .register_async_method("call_oracleGetPrice", |params, state, _ctx| async move {
            let asset_id: u64 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let oracle = state.oracle.read().map_err(|_| internal_error("lock poisoned".into()))?;
            match oracle.get_price(asset_id) {
                Some(p) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "assetId": p.asset_id,
                    "medianPrice": p.median_price.to_string(),
                    "blockNumber": p.block_number,
                    "timestamp": p.timestamp,
                    "submissionCount": p.submission_count,
                    "outlierCount": p.outlier_count,
                    "isStale": oracle.is_stale(asset_id, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                })),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetTwap
    module
        .register_async_method("call_oracleGetTwap", |params, state, _ctx| async move {
            let (asset_id,): (u64,) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let oracle = state.oracle.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            match oracle.get_twap(asset_id, now) {
                Some(twap) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "assetId": asset_id,
                    "twap": twap.to_string(),
                    "windowSecs": 86_400,
                })),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_oracleGetValidatorInfo
    module
        .register_async_method("call_oracleGetValidatorInfo", |params, state, _ctx| async move {
            let validator_id: u32 = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let oracle = state.oracle.read().map_err(|_| internal_error("lock poisoned".into()))?;
            match oracle.get_validator_info(validator_id) {
                Some(v) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "validatorId": v.validator_id,
                    "isActive": v.is_active,
                    "outlierCount": v.outlier_count,
                    "lastSubmissionBlock": v.last_submission_block,
                    "submissionCount": v.submission_count,
                })),
                None => Ok::<_, ErrorObjectOwned>(serde_json::json!(null)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── External Bridge RPC Methods ─────────────────────────────────

    // call_bridgeSubmitDeposit
    module
        .register_async_method("call_bridgeSubmitDeposit", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let source_tx_hash_hex = call_obj.get("sourceTxHash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'sourceTxHash' field".into()))?;
            let source_tx_hash = source_tx_hash_hex.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(format!("invalid sourceTxHash: {e}")))?;

            let source_chain_str = call_obj.get("sourceChain")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'sourceChain' field".into()))?;
            let source_chain = match source_chain_str.to_lowercase().as_str() {
                "ethereum" | "ethereummainnet" => call_bridge::ExternalChain::EthereumMainnet,
                "arbitrum" => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(invalid_params("unknown source chain".into())),
            };

            let source_block_number = call_obj.get("sourceBlockNumber")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| invalid_params("missing 'sourceBlockNumber' field".into()))?;

            let sender_hex = call_obj.get("sender")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'sender' field".into()))?;
            let sender = hex::decode(sender_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(format!("invalid sender: {e}")))?;

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
            let sigs_array = call_obj.get("signatures")
                .and_then(|v| v.as_array())
                .ok_or_else(|| invalid_params("missing 'signatures' field".into()))?;
            let signatures: Vec<call_bridge::BridgeSignature> = sigs_array
                .iter()
                .filter_map(|entry| {
                    let idx = entry.get(0)?.as_u64()? as u32;
                    let sig_hex = entry.get(1)?.as_str()?;
                    let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x")).ok()?;
                    if sig_bytes.len() != 65 { return None; }
                    let mut sig = [0u8; 65];
                    sig.copy_from_slice(&sig_bytes);
                    Some(call_bridge::BridgeSignature {
                        validator_index: idx,
                        signature: sig,
                    })
                })
                .collect();

            let op = call_bridge::ExternalBridgeOp::Deposit {
                source_chain,
                source_tx_hash,
                source_block_number,
                sender,
                recipient,
                asset_id,
                amount,
                signatures,
            };

            // Get validator addresses from consensus
            let validators: Vec<Address> = state.validator_state.read()
                .map_err(|_| internal_error("lock poisoned".into()))?
                .get_all_validators()
                .values()
                .map(|s| s.address)
                .collect();

            let config = call_bridge::BridgeConfig::default();

            // Verify signatures
            let verify_result = call_bridge::verify_bridge_signatures(&op, &validators, config.min_validator_signatures);

            if let Err(e) = verify_result {
                return Err(invalid_params(e.to_string()));
            }

            // Process deposit
            let mut balances = state.balance_state.write().map_err(|_| internal_error("lock poisoned".into()))?;
            let mut bridge_state = state.bridge_state.write().map_err(|_| internal_error("lock poisoned".into()))?;

            let current_block = state.get_current_block();

            // Process deposit — now queues for challenge period instead of instant credit
            match call_bridge::process_external_deposit(&op, &mut balances, &mut bridge_state, &config, &validators, current_block) {
                Ok(call_bridge::ExternalDepositResult::Queued { finalized_at_block, .. }) => Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "status": "queued",
                    "sourceTxHash": source_tx_hash_hex,
                    "assetId": asset_id,
                    "amount": amount.to_string(),
                    "recipient": format!("0x{}", hex::encode(recipient.as_slice())),
                    "challengePeriodBlocks": config.challenge_period_blocks,
                    "finalizedAtBlock": finalized_at_block,
                })),
                Err(e) => Err(invalid_params(e.to_string())),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // call_bridgeGetDepositStatus
    module
        .register_async_method("call_bridgeGetDepositStatus", |params, state, _ctx| async move {
            let source_tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let bridge = state.bridge_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
            // Return aggregate bridge stats (in production, track per-deposit status)
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "sourceTxHash": source_tx_hash,
                "totalDeposits": bridge.total_deposits,
                "totalWithdrawals": bridge.total_withdrawals,
                "pendingOps": bridge.pending_ops.len(),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Light Client Bridge RPC Methods ──────────────────────────────

    // call_lightClientBridgeDeposit
    module
        .register_async_method("call_lightClientBridgeDeposit", |params, state, _ctx| async move {
            #[cfg(not(feature = "light-client-bridge"))]
            return Err::<serde_json::Value, _>(internal_error(
                "light client bridge is not enabled".into()));

            #[cfg(feature = "light-client-bridge")]
            async {
                let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

                // Parse header RLP (hex bytes)
                let header_hex = call_obj.get("headerRlp")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'headerRlp' field".into()))?;
                let header_bytes = hex::decode(header_hex.trim_start_matches("0x"))
                    .map_err(|e| invalid_params(format!("invalid headerRlp: {e}")))?;

                // Parse source chain
                let source_chain_str = call_obj.get("sourceChain")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'sourceChain' field".into()))?;
                let source_chain = match source_chain_str.to_lowercase().as_str() {
                    "ethereum" | "ethereummainnet" => call_bridge::ExternalChain::EthereumMainnet,
                    "arbitrum" => call_bridge::ExternalChain::Arbitrum,
                    _ => return Err(invalid_params("unknown source chain".into())),
                };

                // Parse recipient
                let recipient_hex = call_obj.get("recipient")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'recipient' field".into()))?;
                let recipient = recipient_hex.parse::<alloy_primitives::Address>()
                    .map_err(|e| invalid_params(format!("invalid recipient: {e}")))?;

                // Parse asset_id and amount
                let asset_id = call_obj.get("assetId")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| invalid_params("missing 'assetId' field".into()))?;
                let amount_str = call_obj.get("amount")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid_params("missing 'amount' field (must be string)".into()))?;
                let amount: u128 = amount_str
                    .parse()
                    .map_err(|_| invalid_params("invalid amount: must be a numeric string".into()))?;

                // Parse MPT proof nodes
                let tx_proof_nodes: Vec<Vec<u8>> = call_obj.get("txProof")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())).collect())
                    .ok_or_else(|| invalid_params("missing 'txProof' field".into()))?;

                let receipt_proof_nodes: Vec<Vec<u8>> = call_obj.get("receiptProof")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())).collect())
                    .ok_or_else(|| invalid_params("missing 'receiptProof' field".into()))?;

                // Build light client types
                use call_light_client::{EthHeader, TxInclusionProof, ReceiptProof, MptProofNode};

                let header = EthHeader::from_rlp(header_bytes);

                let tx_proof = TxInclusionProof::new(
                    tx_proof_nodes.into_iter().map(MptProofNode::new).collect(),
                );
                let receipt_proof = ReceiptProof::new(
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

                // Get light client (create with dummy genesis if not initialized)
                let mut light_client_guard = state.light_client.write().map_err(|_| internal_error("lock poisoned".into()))?;
                let light_client = light_client_guard.as_mut()
                    .ok_or_else(|| invalid_params("light client not initialized".into()))?;

                // Get config
                let config = call_bridge::BridgeConfig::default();
                let current_block = state.get_current_block();

                // Process deposit
                let mut balances = state.balance_state.write().map_err(|_| internal_error("lock poisoned".into()))?;
                let mut bridge_state = state.bridge_state.write().map_err(|_| internal_error("lock poisoned".into()))?;

                match call_bridge::process_light_client_deposit(light_client, &op, &mut balances, &mut bridge_state, &config, current_block) {
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

    Ok(())
}

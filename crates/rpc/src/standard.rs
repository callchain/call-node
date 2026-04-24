//! Standard Ethereum JSON-RPC endpoints (per spec §11.1)

use crate::handlers::{RpcState, invalid_params, internal_error};
use call_primitives::Address;
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::Arc;

/// Register standard RPC methods
pub fn register_standard_rpc(module: &mut RpcModule<Arc<RpcState>>) -> Result<(), ErrorObjectOwned> {
    // eth_getBalance
    module
        .register_async_method("eth_getBalance", |params, state, _ctx| async move {
            let address: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address
                .parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let balance = state.get_evm_balance(&addr);
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", balance))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_call
    module
        .register_async_method("eth_call", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let from = call_obj.get("from")
                .and_then(|v| v.as_str())
                .map(|s| s.parse::<alloy_primitives::Address>())
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or_default();
            let to = call_obj.get("to")
                .and_then(|v| v.as_str())
                .map(|s| s.parse::<alloy_primitives::Address>())
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?;
            let value = call_obj.get("value")
                .and_then(|v| v.as_str())
                .map(|s| alloy_primitives::U256::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or_default();
            let data = call_obj.get("data")
                .and_then(|v| v.as_str())
                .map(|s| hex::decode(s.trim_start_matches("0x")))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .map(alloy_primitives::Bytes::from)
                .unwrap_or_default();
            let gas = call_obj.get("gas")
                .and_then(|v| v.as_str())
                .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or(30_000_000);
            let gas_price = call_obj.get("gasPrice")
                .and_then(|v| v.as_str())
                .map(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or(10);

            let caller = from;
            let to_addr = to;

            match state.execute_evm_call(caller, to_addr, value, data, gas, gas_price) {
                Ok(result) => {
                    if result.success {
                        Ok::<_, ErrorObjectOwned>(format!("0x{}", hex::encode(&result.output)))
                    } else {
                        Err(internal_error("execution reverted".into()))
                    }
                }
                Err(e) => Err(internal_error(e)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_sendRawTransaction
    module
        .register_async_method("eth_sendRawTransaction", |params, state, _ctx| async move {
            let raw_tx_hex: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let raw_tx_bytes = hex::decode(raw_tx_hex.trim_start_matches("0x"))
                .map_err(|e| invalid_params(e.to_string()))?;

            match state.submit_evm_tx(&raw_tx_bytes) {
                Ok(tx_hash) => {
                    Ok::<_, ErrorObjectOwned>(format!("0x{}", hex::encode(tx_hash)))
                }
                Err(e) => Err(invalid_params(e)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionReceipt
    module
        .register_async_method("eth_getTransactionReceipt", |params, state, _ctx| async move {
            let tx_hash: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let hash = tx_hash
                .parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            match state.get_receipt(&hash) {
                Some(receipt) => Ok::<_, ErrorObjectOwned>(Some(receipt_to_json(&receipt))),
                None => Ok::<_, ErrorObjectOwned>(None),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_blockNumber
    module
        .register_async_method("eth_blockNumber", |_params, state, _ctx| async move {
            let block = state.get_current_block();
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", block))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getLogs
    module
        .register_async_method("eth_getLogs", |params, state, _ctx| async move {
            let filter: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let addresses: Vec<call_primitives::Address> = filter.get("address")
                .map(|v| match v {
                    serde_json::Value::String(s) => vec![s.parse::<alloy_primitives::Address>()]
                        .into_iter().filter_map(|r| r.ok()).collect(),
                    serde_json::Value::Array(arr) => arr.iter()
                        .filter_map(|x| x.as_str()
                            .and_then(|s| s.parse::<alloy_primitives::Address>().ok()))
                        .collect(),
                    _ => vec![],
                })
                .unwrap_or_default();

            let logs: Vec<serde_json::Value> = if let Some(indexed) = state.lookup_logs_by_address(&addresses) {
                // Use log index for O(1) per-address lookup
                let receipts = state.receipts.read().map_err(|_| internal_error("lock poisoned".into()))?;
                indexed
                    .into_iter()
                    .filter_map(|(_block, tx_hash, log_idx)| {
                        let receipt = receipts.get(&tx_hash)?;
                        let log = receipt.logs.get(log_idx)?;
                        Some(serde_json::json!({
                            "transactionHash": format!("0x{}", hex::encode(tx_hash)),
                            "address": format!("{:?}", log.address),
                            "topics": log.topics.iter().map(|t| format!("0x{}", hex::encode(t.as_slice()))).collect::<Vec<_>>(),
                            "data": format!("0x{}", hex::encode(&log.data)),
                        }))
                    })
                    .collect()
            } else {
                // Fallback: scan all receipts when no address filter (limit to 10k receipts)
                const MAX_RECEIPTS_SCAN: usize = 10_000;
                let receipts = state.get_all_receipts();
                receipts
                    .iter()
                    .take(MAX_RECEIPTS_SCAN)
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
                    .collect()
            };

            Ok::<_, ErrorObjectOwned>(logs)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getProof
    module
        .register_async_method("eth_getProof", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;

            let address_str = call_obj.get("address")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_params("missing 'address' field".into()))?;
            let address = address_str.parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;

            let storage_keys: Vec<String> = call_obj.get("storageKeys")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();

            // Read account state
            let cp_address: Address = Address::from_slice(address.as_slice());
            let balance = state.get_evm_balance(&cp_address);
            let (nonce, code_hash) = {
                let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
                let nonce = evm.get_nonce(&cp_address);
                let code = evm.get_code(&cp_address);
                let code_hash = call_crypto::keccak256(&code);
                (nonce, code_hash)
            };
            let state_root = {
                let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?;
                evm.compute_state_root()
            };

            // Build storage proof entries
            let storage_proof: Vec<serde_json::Value> = storage_keys
                .iter()
                .filter_map(|key_hex| {
                    key_hex.strip_prefix("0x")
                        .and_then(|k| alloy_primitives::U256::from_str_radix(k, 16).ok())
                        .map(|key| {
                            let value = state.evm_state.read().ok().map(|s| s.get_storage(&cp_address, key)).unwrap_or_default();
                            serde_json::json!({
                                "key": key_hex,
                                "value": format!("0x{:x}", value),
                                "proof": [format!("0x{}", hex::encode(state_root))],
                            })
                        })
                })
                .collect();

            let account_proof = vec![format!("0x{}", hex::encode(state_root))];

            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "address": address_str,
                "balance": format!("0x{:x}", balance),
                "codeHash": format!("{code_hash:?}"),
                "nonce": format!("0x{nonce:x}"),
                "stateRoot": format!("0x{}", hex::encode(state_root)),
                "accountProof": account_proof,
                "storageProof": storage_proof,
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_chainId
    module
        .register_async_method("eth_chainId", |_params, state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", state.chain_id))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_gasPrice
    module
        .register_async_method("eth_gasPrice", |_params, state, _ctx| async move {
            let base_fee = state.fee_params.read().map_err(|_| internal_error("lock poisoned".into()))?.base_fee;
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", base_fee))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_syncing
    module
        .register_async_method("eth_syncing", |_params, _state, _ctx| async move {
            // Return false when node is fully synced; object when syncing.
            // RPC state has no sync progress tracking — assume fully synced.
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Bool(false))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionCount
    module
        .register_async_method("eth_getTransactionCount", |params, state, _ctx| async move {
            let (address, _block_tag): (String, Option<String>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address.parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_address = Address::from_slice(addr.as_slice());
            let nonce = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?.get_nonce(&cp_address);
            Ok::<_, ErrorObjectOwned>(format!("0x{nonce:x}"))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getCode
    module
        .register_async_method("eth_getCode", |params, state, _ctx| async move {
            let (address, _block_tag): (String, Option<String>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address.parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_address = Address::from_slice(addr.as_slice());
            let code = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?.get_code(&cp_address);
            Ok::<_, ErrorObjectOwned>(format!("0x{}", hex::encode(&code)))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getStorageAt
    module
        .register_async_method("eth_getStorageAt", |params, state, _ctx| async move {
            let (address, key_hex, _block_tag): (String, String, Option<String>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address.parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let key = alloy_primitives::U256::from_str_radix(key_hex.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_address = Address::from_slice(addr.as_slice());
            let value = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?.get_storage(&cp_address, key);
            Ok::<_, ErrorObjectOwned>(format!("0x{:064x}", value))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_estimateGas
    module
        .register_async_method("eth_estimateGas", |params, state, _ctx| async move {
            let call_obj: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let from = call_obj.get("from")
                .and_then(|v| v.as_str())
                .map(|s| s.parse::<alloy_primitives::Address>())
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or_default();
            let to = call_obj.get("to")
                .and_then(|v| v.as_str())
                .map(|s| s.parse::<alloy_primitives::Address>())
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?;
            let value = call_obj.get("value")
                .and_then(|v| v.as_str())
                .map(|s| alloy_primitives::U256::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or_default();
            let data = call_obj.get("data")
                .and_then(|v| v.as_str())
                .map(|s| hex::decode(s.trim_start_matches("0x")))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .map(alloy_primitives::Bytes::from)
                .unwrap_or_default();
            let gas = call_obj.get("gas")
                .and_then(|v| v.as_str())
                .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or(30_000_000);
            let gas_price = call_obj.get("gasPrice")
                .and_then(|v| v.as_str())
                .map(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16))
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?
                .unwrap_or(10);

            match state.execute_evm_call(from, to, value, data, gas, gas_price) {
                Ok(result) => {
                    if result.success {
                        Ok::<_, ErrorObjectOwned>(format!("0x{:x}", result.gas_used))
                    } else {
                        Err(internal_error("execution reverted".into()))
                    }
                }
                Err(e) => Err(internal_error(e)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockByNumber
    module
        .register_async_method("eth_getBlockByNumber", |params, state, _ctx| async move {
            let (block_tag, _full_txs): (String, Option<bool>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();

            let block_number = match block_tag.as_str() {
                "latest" | "pending" | "safe" | "finalized" => current,
                hex if hex.starts_with("0x") => u64::from_str_radix(&hex[2..], 16).unwrap_or(current),
                num => num.parse::<u64>().unwrap_or(current),
            };

            if block_number != current {
                // Historical blocks are not stored in RPC state
                return Ok::<_, ErrorObjectOwned>(serde_json::Value::Null);
            }

            // Minimal block response for current block
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "number": format!("0x{:x}", block_number),
                "hash": format!("0x{}", hex::encode([0u8; 32])),
                "parentHash": format!("0x{}", hex::encode([0u8; 32])),
                "timestamp": format!("0x{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                "gasLimit": "0x1c9c380",
                "gasUsed": "0x0",
                "transactions": [],
                "logsBloom": format!("0x{}", hex::encode([0u8; 256])),
                "miner": format!("0x{}", hex::encode([0u8; 20])),
                "difficulty": "0x0",
                "totalDifficulty": "0x0",
                "nonce": "0x0000000000000000",
                "sha3Uncles": format!("0x{}", hex::encode([0u8; 32])),
                "receiptsRoot": format!("0x{}", hex::encode([0u8; 32])),
                "transactionsRoot": format!("0x{}", hex::encode([0u8; 32])),
                "stateRoot": format!("0x{}", hex::encode([0u8; 32])),
                "size": "0x0",
                "extraData": "0x",
                "mixHash": format!("0x{}", hex::encode([0u8; 32])),
                "baseFeePerGas": format!("0x{:x}", state.fee_params.read().map_err(|_| internal_error("lock poisoned".into()))?.base_fee),
            }))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockByHash
    module
        .register_async_method("eth_getBlockByHash", |_params, _state, _ctx| async move {
            // Blocks are not stored in RPC state; full node stores them on disk.
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionByHash
    module
        .register_async_method("eth_getTransactionByHash", |params, state, _ctx| async move {
            let tx_hash_str: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let tx_hash = tx_hash_str.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;

            // Try to find receipt (transaction may have been executed)
            if let Some(receipt) = state.get_receipt(&tx_hash) {
                return Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "hash": tx_hash_str,
                    "blockNumber": format!("0x{:x}", receipt.block_number),
                    "status": if matches!(receipt.status, call_primitives::ExecutionStatus::Success) { "0x1" } else { "0x0" },
                    "gasUsed": format!("0x{:x}", receipt.gas_used),
                    "from": format!("{:?}", receipt.gas_payer),
                }));
            }

            // Not found in receipts — may still be pending in mempool
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    Ok(())
}

fn receipt_to_json(receipt: &call_protocol::ProtocolReceipt) -> serde_json::Value {
    let status = match &receipt.status {
        call_primitives::ExecutionStatus::Success => "0x1",
        call_primitives::ExecutionStatus::Reverted { .. } => "0x0",
    };
    let logs: Vec<serde_json::Value> = receipt
        .logs
        .iter()
        .map(|log| {
            serde_json::json!({
                "address": format!("{:?}", log.address),
                "topics": log.topics.iter().map(|t| format!("{:?}", t)).collect::<Vec<_>>(),
                "data": format!("0x{}", hex::encode(&log.data)),
            })
        })
        .collect();
    serde_json::json!({
        "transactionHash": format!("{:?}", receipt.tx_hash),
        "status": status,
        "gasUsed": format!("0x{:x}", receipt.gas_used),
        "gasPayer": format!("{:?}", receipt.gas_payer),
        "feeCurrency": format!("{:?}", receipt.fee_currency),
        "feeAmount": format!("0x{:x}", receipt.fee_amount),
        "blockNumber": if receipt.block_number == 0 { "pending".to_string() } else { format!("0x{:x}", receipt.block_number) },
        "pending": receipt.block_number == 0,
        "logs": logs,
    })
}

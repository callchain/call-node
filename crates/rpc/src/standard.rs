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
            let logs = get_logs_from_filter(&filter, &state)?;
            Ok::<_, ErrorObjectOwned>(logs)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_newFilter
    module
        .register_async_method("eth_newFilter", |params, state, _ctx| async move {
            let filter: serde_json::Value = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();

            let from_block = filter.get("fromBlock")
                .and_then(|v| v.as_str())
                .map(|s| parse_block_tag(s, current))
                .unwrap_or(current);
            let to_block = filter.get("toBlock")
                .and_then(|v| v.as_str())
                .map(|s| parse_block_tag(s, current))
                .unwrap_or(current);
            let block_hash = filter.get("blockHash")
                .and_then(|v| v.as_str())
                .map(|s| s.parse::<alloy_primitives::B256>())
                .transpose()
                .map_err(|e| invalid_params(e.to_string()))?;

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

            let topics: Vec<Option<Vec<call_primitives::Hash>>> = filter.get("topics")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().map(|entry| {
                    match entry {
                        serde_json::Value::String(s) => {
                            s.parse::<alloy_primitives::B256>().ok()
                                .map(|h| vec![call_primitives::Hash::from(h.0)])
                        }
                        serde_json::Value::Array(arr) => {
                            let hashes: Vec<_> = arr.iter()
                                .filter_map(|x| x.as_str())
                                .filter_map(|s| s.parse::<alloy_primitives::B256>().ok())
                                .map(|h| call_primitives::Hash::from(h.0))
                                .collect();
                            if hashes.is_empty() { None } else { Some(hashes) }
                        }
                        serde_json::Value::Null => None,
                        _ => None,
                    }
                }).collect())
                .unwrap_or_default();

            // If blockHash is given, override fromBlock/toBlock to that block's height
            let (from_block, to_block) = if let Some(hash) = block_hash {
                let cp_hash = call_primitives::Hash::from(hash.0);
                let height = state.block_hash_index.read()
                    .ok()
                    .and_then(|idx| idx.get(&cp_hash).copied())
                    .unwrap_or(current);
                (height, height)
            } else {
                (from_block, to_block)
            };

            let filter_obj = crate::handlers::state::Filter::Log {
                from_block,
                to_block,
                addresses,
                topics,
                last_block: from_block.saturating_sub(1),
            };
            let id = state.filter_manager.create_filter(filter_obj);
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", id))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_newBlockFilter
    module
        .register_async_method("eth_newBlockFilter", |_params, state, _ctx| async move {
            let current = state.get_current_block();
            let filter = crate::handlers::state::Filter::Block { last_height: current.saturating_sub(1) };
            let id = state.filter_manager.create_filter(filter);
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", id))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_newPendingTransactionFilter
    module
        .register_async_method("eth_newPendingTransactionFilter", |_params, state, _ctx| async move {
            let filter = crate::handlers::state::Filter::PendingTransaction { seen: std::collections::HashSet::new() };
            let id = state.filter_manager.create_filter(filter);
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", id))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getFilterChanges
    module
        .register_async_method("eth_getFilterChanges", |params, state, _ctx| async move {
            let id_hex: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let id = u64::from_str_radix(id_hex.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;

            let Some(filter) = state.filter_manager.get_filter(id) else {
                return Err::<serde_json::Value, _>(invalid_params("filter not found".into()));
            };

            let result = match filter {
                crate::handlers::state::Filter::Log { from_block, to_block, addresses, topics, last_block } => {
                    let current = state.get_current_block();
                    let effective_from = last_block.saturating_add(1).max(from_block);
                    let effective_to = to_block.min(current);
                    let mut results = Vec::new();
                    for block_num in effective_from..=effective_to {
                        let block_receipts = state.get_receipts_by_block(block_num);
                        for receipt in block_receipts {
                            for (log_idx, log) in receipt.logs.iter().enumerate() {
                                if !addresses.is_empty() && !addresses.contains(&log.address) {
                                    continue;
                                }
                                if !topics.is_empty() {
                                    let mut matched = true;
                                    for (topic_idx, topic_filter) in topics.iter().enumerate() {
                                        if let Some(filter_hashes) = topic_filter {
                                            if let Some(log_topic) = log.topics.get(topic_idx) {
                                                if !filter_hashes.contains(log_topic) {
                                                    matched = false;
                                                    break;
                                                }
                                            } else {
                                                matched = false;
                                                break;
                                            }
                                        }
                                    }
                                    if !matched { continue; }
                                }
                                results.push(log_to_json(log, &receipt, log_idx));
                            }
                        }
                    }
                    // Update cursor
                    let new_filter = crate::handlers::state::Filter::Log {
                        from_block, to_block, addresses, topics,
                        last_block: effective_to,
                    };
                    state.filter_manager.update_filter(id, new_filter);
                    serde_json::Value::Array(results)
                }
                crate::handlers::state::Filter::Block { last_height } => {
                    let current = state.get_current_block();
                    let mut results = Vec::new();
                    for h in last_height.saturating_add(1)..=current {
                        if let Some(block) = state.load_block(h) {
                            results.push(serde_json::Value::String(format!("0x{}", hex::encode(block.header.hash().as_slice()))));
                        }
                    }
                    state.filter_manager.update_filter(id, crate::handlers::state::Filter::Block { last_height: current });
                    serde_json::Value::Array(results)
                }
                crate::handlers::state::Filter::PendingTransaction { mut seen } => {
                    // Collect pending tx hashes from EVM mempool only
                    let mut pending = Vec::new();
                    {
                        let mempool = state.mempool.read().map_err(|_| internal_error("lock poisoned".into()))?;
                        for entry in mempool.evm_pool.iter() {
                            let hash = call_crypto::keccak256(&entry.data);
                            let tx_hash = call_primitives::TxHash::from(hash.0);
                            if seen.insert(tx_hash) {
                                pending.push(format!("0x{}", hex::encode(hash.as_slice())));
                            }
                        }
                    }
                    state.filter_manager.update_filter(id, crate::handlers::state::Filter::PendingTransaction { seen });
                    serde_json::Value::Array(pending.into_iter().map(serde_json::Value::String).collect())
                }
            };

            Ok::<_, ErrorObjectOwned>(result)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getFilterLogs
    module
        .register_async_method("eth_getFilterLogs", |params, state, _ctx| async move {
            let id_hex: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let id = u64::from_str_radix(id_hex.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;

            let Some(filter) = state.filter_manager.get_filter(id) else {
                return Err::<serde_json::Value, _>(invalid_params("filter not found".into()));
            };

            let result = match filter {
                crate::handlers::state::Filter::Log { from_block, to_block, addresses, topics, .. } => {
                    let current = state.get_current_block();
                    let effective_to = to_block.min(current);
                    let mut results = Vec::new();
                    for block_num in from_block..=effective_to {
                        let block_receipts = state.get_receipts_by_block(block_num);
                        for receipt in block_receipts {
                            for (log_idx, log) in receipt.logs.iter().enumerate() {
                                if !addresses.is_empty() && !addresses.contains(&log.address) {
                                    continue;
                                }
                                if !topics.is_empty() {
                                    let mut matched = true;
                                    for (topic_idx, topic_filter) in topics.iter().enumerate() {
                                        if let Some(filter_hashes) = topic_filter {
                                            if let Some(log_topic) = log.topics.get(topic_idx) {
                                                if !filter_hashes.contains(log_topic) {
                                                    matched = false;
                                                    break;
                                                }
                                            } else {
                                                matched = false;
                                                break;
                                            }
                                        }
                                    }
                                    if !matched { continue; }
                                }
                                results.push(log_to_json(log, &receipt, log_idx));
                            }
                        }
                    }
                    serde_json::Value::Array(results)
                }
                _ => serde_json::Value::Array(vec![]),
            };

            Ok::<_, ErrorObjectOwned>(result)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_uninstallFilter
    module
        .register_async_method("eth_uninstallFilter", |params, state, _ctx| async move {
            let id_hex: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let id = u64::from_str_radix(id_hex.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;
            let removed = state.filter_manager.remove_filter(id);
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Bool(removed))
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
            let gas_price = base_fee.saturating_add(call_protocol::gas::MIN_PRIORITY_FEE_PER_GAS);
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", gas_price))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_maxPriorityFeePerGas
    module
        .register_async_method("eth_maxPriorityFeePerGas", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", call_protocol::gas::MIN_PRIORITY_FEE_PER_GAS))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_feeHistory
    module
        .register_async_method("eth_feeHistory", |params, state, _ctx| async move {
            let (block_count, newest_block, reward_percentiles): (String, String, Option<Vec<f64>>) =
                params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let block_count = u64::from_str_radix(block_count.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;
            let newest_block_num = match newest_block.as_str() {
                "latest" | "pending" => state.get_current_block(),
                hex if hex.starts_with("0x") => u64::from_str_radix(&hex[2..], 16).unwrap_or(0),
                _ => newest_block.parse().unwrap_or(0),
            };

            let history = state.fee_history.read().map_err(|_| internal_error("lock poisoned".into()))?;
            let entries: Vec<_> = history.iter().rev().take(block_count as usize).cloned().collect();
            let oldest_block = entries.last().map(|(n, _)| *n).unwrap_or(newest_block_num);

            let mut base_fee_per_gas = Vec::new();
            let mut gas_used_ratio = Vec::new();
            let mut reward = Vec::new();

            for (_, entry) in entries.iter().rev() {
                base_fee_per_gas.push(format!("0x{:x}", entry.base_fee));
                gas_used_ratio.push(entry.gas_used_ratio);
                let percentiles = reward_percentiles.as_ref().map(|p| p.len()).unwrap_or(0);
                let mut block_rewards = Vec::new();
                for _ in 0..percentiles {
                    block_rewards.push(format!("0x1"));
                }
                if !block_rewards.is_empty() {
                    reward.push(block_rewards);
                }
            }

            let next_base_fee = state.fee_params.read().map_err(|_| internal_error("lock poisoned".into()))?.base_fee;
            base_fee_per_gas.push(format!("0x{:x}", next_base_fee));

            let mut result = serde_json::json!({
                "oldestBlock": format!("0x{:x}", oldest_block),
                "baseFeePerGas": base_fee_per_gas,
                "gasUsedRatio": gas_used_ratio,
            });
            if !reward.is_empty() {
                result["reward"] = serde_json::Value::Array(
                    reward
                        .into_iter()
                        .map(|r| serde_json::Value::Array(r.into_iter().map(serde_json::Value::String).collect()))
                        .collect()
                );
            }
            Ok::<_, ErrorObjectOwned>(result)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_syncing
    module
        .register_async_method("eth_syncing", |_params, state, _ctx| async move {
            let progress = state.sync_progress.read().map_err(|_| internal_error("lock poisoned".into()))?.clone();
            match progress {
                Some(p) if p.current_block < p.highest_block => {
                    Ok::<_, ErrorObjectOwned>(serde_json::json!({
                        "startingBlock": format!("0x{:x}", p.starting_block),
                        "currentBlock": format!("0x{:x}", p.current_block),
                        "highestBlock": format!("0x{:x}", p.highest_block),
                    }))
                }
                _ => Ok::<_, ErrorObjectOwned>(serde_json::Value::Bool(false)),
            }
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionCount
    module
        .register_async_method("eth_getTransactionCount", |params, state, _ctx| async move {
            let (address, block_tag): (String, Option<String>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let addr = address.parse::<alloy_primitives::Address>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_address = Address::from_slice(addr.as_slice());
            let committed_nonce = state.evm_state.read().map_err(|_| internal_error("lock poisoned".into()))?.get_nonce(&cp_address);
            let nonce = if block_tag.as_deref() == Some("pending") {
                let mempool = state.mempool.read().map_err(|_| internal_error("lock poisoned".into()))?;
                committed_nonce.max(mempool.evm_pool.get_address_nonce(&cp_address))
            } else {
                committed_nonce
            };
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
            let (block_tag, full_txs): (String, Option<bool>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();
            let block_number = parse_block_tag(&block_tag, current);

            if let Some(block) = state.load_block(block_number) {
                return Ok::<_, ErrorObjectOwned>(block_to_json(&block, &state, full_txs.unwrap_or(false)));
            }

            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockByHash
    module
        .register_async_method("eth_getBlockByHash", |params, state, _ctx| async move {
            let (block_hash_str, full_txs): (String, Option<bool>) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let hash = block_hash_str.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_hash = call_primitives::Hash::from(hash.0);

            if let Some(block) = state.load_block_by_hash(&cp_hash) {
                return Ok::<_, ErrorObjectOwned>(block_to_json(&block, &state, full_txs.unwrap_or(false)));
            }

            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionByHash
    module
        .register_async_method("eth_getTransactionByHash", |params, state, _ctx| async move {
            let tx_hash_str: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let tx_hash = tx_hash_str.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_hash = call_primitives::TxHash::from(tx_hash.0);

            // Try to find receipt (transaction may have been executed)
            if let Some(receipt) = state.get_receipt(&cp_hash) {
                let block_number = receipt.block_number;
                if let Some(block) = state.load_block(block_number) {
                    // Search EVM txs
                    for (idx, raw) in block.evm_txs.iter().enumerate() {
                        let raw_hash = call_crypto::keccak256(raw);
                        if raw_hash == cp_hash {
                            if let Some(json) = evm_raw_tx_to_json(raw, receipt.block_hash, block_number, idx as u64) {
                                return Ok::<_, ErrorObjectOwned>(json);
                            }
                        }
                    }
                    // Protocol txs are no longer accepted via mempool; blocks may still
                    // contain them from consensus, but RPC lookup is EVM-only.
                }
                // Fallback if block not available: return receipt-based minimal info
                return Ok::<_, ErrorObjectOwned>(serde_json::json!({
                    "hash": tx_hash_str,
                    "blockHash": format!("0x{}", hex::encode(receipt.block_hash.as_slice())),
                    "blockNumber": format!("0x{:x}", receipt.block_number),
                    "transactionIndex": format!("0x{:x}", receipt.transaction_index),
                    "status": if matches!(receipt.status, call_primitives::ExecutionStatus::Success) { "0x1" } else { "0x0" },
                    "gasUsed": format!("0x{:x}", receipt.gas_used),
                    "from": format!("{:?}", receipt.gas_payer),
                    "to": receipt.to.map(|a| format!("{:?}", a)).unwrap_or_else(|| "0x".to_string()),
                }));
            }

            // Not found in receipts — may still be pending in mempool
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockReceipts
    module
        .register_async_method("eth_getBlockReceipts", |params, state, _ctx| async move {
            let block_tag: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();
            let block_number = parse_block_tag(&block_tag, current);

            let receipts = state.get_receipts_by_block(block_number);
            let json_receipts: Vec<serde_json::Value> = receipts
                .iter()
                .map(|r| receipt_to_json(r))
                .collect();
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Array(json_receipts))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockTransactionCountByHash
    module
        .register_async_method("eth_getBlockTransactionCountByHash", |params, state, _ctx| async move {
            let block_hash_str: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let hash = block_hash_str.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_hash = call_primitives::Hash::from(hash.0);

            if let Some(block) = state.load_block_by_hash(&cp_hash) {
                let count = block.evm_txs.len();
                return Ok::<serde_json::Value, ErrorObjectOwned>(serde_json::Value::String(format!("0x{:x}", count)));
            }
            Ok::<serde_json::Value, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getBlockTransactionCountByNumber
    module
        .register_async_method("eth_getBlockTransactionCountByNumber", |params, state, _ctx| async move {
            let block_tag: String = params.one().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();
            let block_number = parse_block_tag(&block_tag, current);

            if let Some(block) = state.load_block(block_number) {
                let count = block.evm_txs.len();
                return Ok::<serde_json::Value, ErrorObjectOwned>(serde_json::Value::String(format!("0x{:x}", count)));
            }
            Ok::<serde_json::Value, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionByBlockHashAndIndex
    module
        .register_async_method("eth_getTransactionByBlockHashAndIndex", |params, state, _ctx| async move {
            let (block_hash_str, idx_str): (String, String) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let hash = block_hash_str.parse::<alloy_primitives::B256>()
                .map_err(|e| invalid_params(e.to_string()))?;
            let cp_hash = call_primitives::Hash::from(hash.0);
            let idx = u64::from_str_radix(idx_str.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;

            if let Some(block) = state.load_block_by_hash(&cp_hash) {
                let evm_len = block.evm_txs.len() as u64;
                if idx < evm_len {
                    let raw = &block.evm_txs[idx as usize];
                    if let Some(json) = evm_raw_tx_to_json(raw, cp_hash, block.header.height, idx) {
                        return Ok::<_, ErrorObjectOwned>(json);
                    }
                }
                // Protocol txs are no longer accepted via mempool; blocks may still
                // contain them from consensus, but RPC lookup is EVM-only.
            }
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getTransactionByBlockNumberAndIndex
    module
        .register_async_method("eth_getTransactionByBlockNumberAndIndex", |params, state, _ctx| async move {
            let (block_tag, idx_str): (String, String) = params.parse().map_err(|e| invalid_params(e.to_string()))?;
            let current = state.get_current_block();
            let block_number = parse_block_tag(&block_tag, current);
            let idx = u64::from_str_radix(idx_str.trim_start_matches("0x"), 16)
                .map_err(|e| invalid_params(e.to_string()))?;

            if let Some(block) = state.load_block(block_number) {
                let block_hash = block.header.hash();
                let evm_len = block.evm_txs.len() as u64;
                if idx < evm_len {
                    let raw = &block.evm_txs[idx as usize];
                    if let Some(json) = evm_raw_tx_to_json(raw, block_hash, block_number, idx) {
                        return Ok::<_, ErrorObjectOwned>(json);
                    }
                }
                // Protocol txs are no longer accepted via mempool; blocks may still
                // contain them from consensus, but RPC lookup is EVM-only.
            }
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_sendTransaction — not supported
    module
        .register_async_method("eth_sendTransaction", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "eth_sendTransaction is not supported; use eth_sendRawTransaction",
                None::<&str>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_createAccessList — not supported
    module
        .register_async_method("eth_createAccessList", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "eth_createAccessList is not supported",
                None::<&str>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // ── Trivial mock methods (no consensus impact) ───────────────────────

    // eth_protocolVersion
    module
        .register_async_method("eth_protocolVersion", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>("1")
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_accounts
    module
        .register_async_method("eth_accounts", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(Vec::<String>::new())
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_mining
    module
        .register_async_method("eth_mining", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Bool(false))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_hashrate
    module
        .register_async_method("eth_hashrate", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_coinbase
    module
        .register_async_method("eth_coinbase", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(format!("0x{}", hex::encode([0u8; 20])))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getUncleCountByBlockNumber
    module
        .register_async_method("eth_getUncleCountByBlockNumber", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getUncleCountByBlockHash
    module
        .register_async_method("eth_getUncleCountByBlockHash", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getUncleByBlockNumberAndIndex
    module
        .register_async_method("eth_getUncleByBlockNumberAndIndex", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getUncleByBlockHashAndIndex
    module
        .register_async_method("eth_getUncleByBlockHashAndIndex", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::Value::Null)
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getWork — not supported
    module
        .register_async_method("eth_getWork", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "Method not found",
                None::<&str>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_submitWork — not supported
    module
        .register_async_method("eth_submitWork", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "Method not found",
                None::<&str>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_submitHashrate — not supported
    module
        .register_async_method("eth_submitHashrate", |_params, _state, _ctx| async move {
            Err::<serde_json::Value, _>(ErrorObjectOwned::owned(
                -32601,
                "Method not found",
                None::<&str>,
            ))
        })
        .map_err(|e| internal_error(e.to_string()))?;

    // eth_getCompilers — deprecated, return empty array
    module
        .register_async_method("eth_getCompilers", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(Vec::<String>::new())
        })
        .map_err(|e| internal_error(e.to_string()))?;

    Ok(())
}

pub(crate) fn receipt_to_json(receipt: &call_protocol::ProtocolReceipt) -> serde_json::Value {
    let (status, revert_reason) = match &receipt.status {
        call_primitives::ExecutionStatus::Success => ("0x1", None),
        call_primitives::ExecutionStatus::Reverted { reason } => ("0x0", Some(reason.as_str())),
    };
    let logs: Vec<serde_json::Value> = receipt
        .logs
        .iter()
        .enumerate()
        .map(|(log_idx, log)| {
            serde_json::json!({
                "address": format!("{:?}", log.address),
                "topics": log.topics.iter().map(|t| format!("{:?}", t)).collect::<Vec<_>>(),
                "data": format!("0x{}", hex::encode(&log.data)),
                "blockNumber": format!("0x{:x}", receipt.block_number),
                "blockHash": format!("0x{}", hex::encode(receipt.block_hash.as_slice())),
                "transactionHash": format!("{:?}", receipt.tx_hash),
                "transactionIndex": format!("0x{:x}", receipt.transaction_index),
                "logIndex": format!("0x{:x}", log_idx),
                "removed": false,
            })
        })
        .collect();
    let mut value = serde_json::json!({
        "transactionHash": format!("{:?}", receipt.tx_hash),
        "transactionIndex": format!("0x{:x}", receipt.transaction_index),
        "blockHash": format!("0x{}", hex::encode(receipt.block_hash.as_slice())),
        "blockNumber": if receipt.block_number == 0 { serde_json::Value::Null } else { serde_json::Value::String(format!("0x{:x}", receipt.block_number)) },
        "from": format!("{:?}", receipt.gas_payer),
        "to": receipt.to.map(|a| format!("{:?}", a)).unwrap_or_else(|| "0x".to_string()),
        "contractAddress": receipt.contract_address.map(|a| format!("{:?}", a)),
        "cumulativeGasUsed": format!("0x{:x}", receipt.cumulative_gas_used),
        "gasUsed": format!("0x{:x}", receipt.gas_used),
        "effectiveGasPrice": format!("0x{:x}", receipt.effective_gas_price),
        "status": status,
        "logsBloom": format!("0x{}", hex::encode(&receipt.logs_bloom)),
        "logs": logs,
        "gasPayer": format!("{:?}", receipt.gas_payer),
        "feeCurrency": format!("{:?}", receipt.fee_currency),
        "feeAmount": format!("0x{:x}", receipt.fee_amount),
    });
    if let Some(reason) = revert_reason {
        value["revertReason"] = serde_json::Value::String(reason.to_string());
    }
    value
}

/// Parse an Ethereum block tag (latest, pending, hex number, decimal number).
fn parse_block_tag(tag: &str, current: u64) -> u64 {
    match tag {
        "latest" | "pending" | "safe" | "finalized" => current,
        hex if hex.starts_with("0x") => u64::from_str_radix(&hex[2..], 16).unwrap_or(current),
        num => num.parse::<u64>().unwrap_or(current),
    }
}

/// Build a JSON representation of a block for eth_getBlockByNumber/Hash.
fn block_to_json(
    block: &call_consensus::Block,
    state: &RpcState,
    full_txs: bool,
) -> serde_json::Value {
    let height = block.header.height;
    let block_hash = block.header.hash();
    let receipts = state.get_receipts_by_block(height);
    let gas_used: u64 = receipts.iter().map(|r| r.gas_used).sum();
    let tx_count = block.evm_txs.len();

    let mut transactions: Vec<serde_json::Value> = Vec::with_capacity(tx_count);

    // EVM transactions
    for (idx, raw) in block.evm_txs.iter().enumerate() {
        let tx_hash = call_crypto::keccak256(raw);
        if full_txs {
            if let Some(json) = evm_raw_tx_to_json(raw, block_hash, height, idx as u64) {
                transactions.push(json);
            } else {
                transactions.push(serde_json::json!({
                    "hash": format!("0x{}", hex::encode(tx_hash.as_slice())),
                    "blockHash": format!("0x{}", hex::encode(block_hash.as_slice())),
                    "blockNumber": format!("0x{:x}", height),
                    "transactionIndex": format!("0x{:x}", idx),
                }));
            }
        } else {
            transactions.push(serde_json::Value::String(format!(
                "0x{}",
                hex::encode(tx_hash.as_slice())
            )));
        }
    }

    // Protocol transactions are no longer accepted via mempool;
    // blocks may still contain them from consensus, but block JSON
    // only surfaces EVM transactions for RPC compatibility.

    // Approximate size: serialized JSON of the block struct
    let size = serde_json::to_vec(block).map(|v| v.len()).unwrap_or(0);

    serde_json::json!({
        "number": format!("0x{:x}", height),
        "hash": format!("0x{}", hex::encode(block_hash.as_slice())),
        "parentHash": format!("0x{}", hex::encode(block.header.parent_hash.as_slice())),
        "timestamp": format!("0x{:x}", block.header.timestamp_millis / 1000),
        "gasLimit": "0x1c9c380",
        "gasUsed": format!("0x{:x}", gas_used),
        "transactions": transactions,
        "logsBloom": format!("0x{}", hex::encode([0u8; 256])),
        "miner": format!("0x{}", hex::encode([0u8; 20])),
        "difficulty": "0x0",
        "totalDifficulty": "0x0",
        "nonce": "0x0000000000000000",
        "sha3Uncles": format!("0x{}", hex::encode([0u8; 32])),
        "receiptsRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
        "transactionsRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
        "stateRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
        "size": format!("0x{:x}", size),
        "extraData": "0x",
        "mixHash": format!("0x{}", hex::encode([0u8; 32])),
        "baseFeePerGas": format!("0x{:x}", state.fee_params.read().map(|p| p.base_fee).unwrap_or(0)),
    })
}

/// Decode raw EVM tx bytes into an Ethereum JSON-RPC transaction object.
fn evm_raw_tx_to_json(
    raw: &[u8],
    block_hash: call_primitives::Hash,
    block_number: u64,
    tx_index: u64,
) -> Option<serde_json::Value> {
    use alloy_consensus::{TxEnvelope, Transaction as _};
    use alloy_rlp::Decodable;

    let envelope = TxEnvelope::decode(&mut &raw[..]).ok()?;
    let tx_hash = call_crypto::keccak256(raw);
    let (nonce, gas_limit, gas_price, to, value, data, chain_id, from) = match &envelope {
        TxEnvelope::Legacy(signed) => {
            let tx = signed.tx();
            let from = signed.recover_signer().ok()?;
            (
                tx.nonce(),
                tx.gas_limit(),
                tx.gas_price().unwrap_or(0),
                tx.to(),
                tx.value(),
                tx.input().clone(),
                tx.chain_id(),
                from,
            )
        }
        TxEnvelope::Eip1559(signed) => {
            let tx = signed.tx();
            let from = signed.recover_signer().ok()?;
            (
                tx.nonce(),
                tx.gas_limit(),
                tx.max_fee_per_gas(),
                tx.to(),
                tx.value(),
                tx.input().clone(),
                tx.chain_id(),
                from,
            )
        }
        _ => return None,
    };

    Some(serde_json::json!({
        "hash": format!("0x{}", hex::encode(tx_hash.as_slice())),
        "nonce": format!("0x{:x}", nonce),
        "blockHash": format!("0x{}", hex::encode(block_hash.as_slice())),
        "blockNumber": format!("0x{:x}", block_number),
        "transactionIndex": format!("0x{:x}", tx_index),
        "from": format!("{:?}", from),
        "to": to.map(|a| format!("{:?}", a)),
        "gas": format!("0x{:x}", gas_limit),
        "gasPrice": format!("0x{:x}", gas_price),
        "value": format!("0x{:x}", value),
        "input": format!("0x{}", hex::encode(&data)),
        "chainId": chain_id.map(|c| format!("0x{:x}", c)),
        "v": "0x0",
        "r": "0x0",
        "s": "0x0",
    }))
}

/// Convert a Protocol LogEntry to ETH JSON-RPC log format.
fn log_to_json(
    log: &call_protocol::LogEntry,
    receipt: &call_protocol::ProtocolReceipt,
    log_index: usize,
) -> serde_json::Value {
    serde_json::json!({
        "address": format!("{:?}", log.address),
        "topics": log.topics.iter().map(|t| format!("0x{}", hex::encode(t.as_slice()))).collect::<Vec<_>>(),
        "data": format!("0x{}", hex::encode(&log.data)),
        "blockNumber": format!("0x{:x}", receipt.block_number),
        "blockHash": format!("0x{}", hex::encode(receipt.block_hash.as_slice())),
        "transactionHash": format!("0x{}", hex::encode(receipt.tx_hash.as_slice())),
        "transactionIndex": format!("0x{:x}", receipt.transaction_index),
        "logIndex": format!("0x{:x}", log_index),
        "removed": false,
    })
}

/// Shared log query logic used by eth_getLogs and eth_getFilterLogs.
fn get_logs_from_filter(
    filter: &serde_json::Value,
    state: &RpcState,
) -> Result<Vec<serde_json::Value>, ErrorObjectOwned> {
    let current = state.get_current_block();

    let from_block = filter.get("fromBlock")
        .and_then(|v| v.as_str())
        .map(|s| parse_block_tag(s, current))
        .unwrap_or(current);
    let to_block = filter.get("toBlock")
        .and_then(|v| v.as_str())
        .map(|s| parse_block_tag(s, current))
        .unwrap_or(current);
    let block_hash = filter.get("blockHash")
        .and_then(|v| v.as_str())
        .map(|s| s.parse::<alloy_primitives::B256>())
        .transpose()
        .map_err(|e| invalid_params(e.to_string()))?;

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

    let topics: Vec<Option<Vec<call_primitives::Hash>>> = filter.get("topics")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|entry| {
            match entry {
                serde_json::Value::String(s) => {
                    s.parse::<alloy_primitives::B256>().ok()
                        .map(|h| vec![call_primitives::Hash::from(h.0)])
                }
                serde_json::Value::Array(arr) => {
                    let hashes: Vec<_> = arr.iter()
                        .filter_map(|x| x.as_str())
                        .filter_map(|s| s.parse::<alloy_primitives::B256>().ok())
                        .map(|h| call_primitives::Hash::from(h.0))
                        .collect();
                    if hashes.is_empty() { None } else { Some(hashes) }
                }
                serde_json::Value::Null => None,
                _ => None,
            }
        }).collect())
        .unwrap_or_default();

    // If blockHash is given, override fromBlock/toBlock to that block's height
    let (from_block, to_block) = if let Some(hash) = block_hash {
        let cp_hash = call_primitives::Hash::from(hash.0);
        let height = state.block_hash_index.read()
            .ok()
            .and_then(|idx| idx.get(&cp_hash).copied())
            .unwrap_or(current);
        (height, height)
    } else {
        (from_block, to_block)
    };

    let mut results = Vec::new();
    for block_num in from_block..=to_block {
        let block_receipts = state.get_receipts_by_block(block_num);
        for receipt in block_receipts {
            for (log_idx, log) in receipt.logs.iter().enumerate() {
                if !addresses.is_empty() && !addresses.contains(&log.address) {
                    continue;
                }
                if !topics.is_empty() {
                    let mut matched = true;
                    for (topic_idx, topic_filter) in topics.iter().enumerate() {
                        if let Some(filter_hashes) = topic_filter {
                            if let Some(log_topic) = log.topics.get(topic_idx) {
                                if !filter_hashes.contains(log_topic) {
                                    matched = false;
                                    break;
                                }
                            } else {
                                matched = false;
                                break;
                            }
                        }
                    }
                    if !matched { continue; }
                }
                results.push(log_to_json(log, &receipt, log_idx));
            }
        }
    }

    Ok(results)
}

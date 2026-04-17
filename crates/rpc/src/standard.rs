//! Standard Ethereum JSON-RPC endpoints (per spec §11.1)

use crate::handlers::{RpcState, invalid_params, internal_error};
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

            let receipts = state.get_all_receipts();
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

    // eth_getProof
    module
        .register_async_method("eth_getProof", |_params, _state, _ctx| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!({
                "address": "0x0000000000000000000000000000000000000000",
                "storageProof": [],
            }))
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
        "logs": logs,
    })
}

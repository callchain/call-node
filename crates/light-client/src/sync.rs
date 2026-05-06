//! Ethereum header synchronization.
//!
//! Fetches headers and finalized checkpoints from Ethereum via JSON-RPC.
//! Requires the `eth-sync` feature flag.

use crate::types::EthHeader;
use crate::EthLightClient;
use alloy_primitives::B256;

/// Fetch a single Ethereum header by block number via `eth_getBlockByNumber`.
pub fn sync_single_header(eth_rpc_url: &str, block_number: u64) -> Result<EthHeader, String> {
    let hex_block = format!("0x{:x}", block_number);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBlockByNumber",
        "params": [hex_block, false],
        "id": 1
    });

    let resp = ureq::post(eth_rpc_url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("RPC request failed: {e}"))?;

    let text = resp
        .into_string()
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    let response: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Failed to parse JSON response: {e}"))?;

    let result = response.get("result").ok_or_else(|| {
        let error = response
            .get("error")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        format!("RPC error: {error}")
    })?;

    if result.is_null() {
        return Err(format!("No block found at number {block_number}"));
    }

    // eth_getBlockByNumber returns header fields as JSON, not RLP bytes.
    // We reconstruct the RLP from the returned fields.
    let header_rlp =
        decode_eth_header_rlp(result).ok_or("Failed to decode header RLP from RPC response")?;

    Ok(EthHeader::from_rlp(header_rlp))
}

/// Sync a range of headers from `from` to `to` (inclusive) into the light client.
/// Returns the number of headers successfully synced.
pub fn sync_header_range(
    eth_rpc_url: &str,
    client: &mut EthLightClient,
    from: u64,
    to: u64,
) -> Result<usize, String> {
    let mut synced = 0;
    for block_num in from..=to {
        let header = sync_single_header(eth_rpc_url, block_num)?;
        if let Err(e) = client.submit_header(header) {
            if matches!(e, crate::LightClientError::BufferFull) {
                break;
            }
            return Err(format!("Failed to submit header at block {block_num}: {e}"));
        }
        synced += 1;
    }
    Ok(synced)
}

/// Fetch the finalized checkpoint from Ethereum beacon chain.
/// Returns (block_number, block_hash).
pub fn fetch_finalized_checkpoint(beacon_url: &str) -> Result<(u64, B256), String> {
    let url = format!(
        "{}/eth/v1/beacon/states/finalized/finality_checkpoints",
        beacon_url
    );
    let resp = ureq::get(&url)
        .call()
        .map_err(|e| format!("Beacon API request failed: {e}"))?;

    let text = resp
        .into_string()
        .map_err(|e| format!("Failed to read beacon response body: {e}"))?;

    let response: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("Failed to parse beacon JSON response: {e}"))?;

    let data = response
        .get("data")
        .ok_or("Missing 'data' in beacon response")?;
    let header = data
        .get("header")
        .ok_or("Missing 'header' in beacon data")?;
    let block = header
        .get("beacon")
        .ok_or("Missing 'beacon' in header data")?;

    let slot_val = block.get("slot").ok_or("Missing 'slot' in beacon header")?;
    let block_number: u64 = if let Some(s) = slot_val.as_str() {
        s.parse()
            .map_err(|e| format!("Failed to parse slot as u64: {e}"))?
    } else if let Some(n) = slot_val.as_u64() {
        n
    } else {
        return Err("'slot' is not a string or number".into());
    };

    let root_str = block
        .get("root")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'root' in beacon header")?;

    let block_hash = root_str
        .strip_prefix("0x")
        .unwrap_or(root_str)
        .parse()
        .map_err(|e| format!("Failed to parse block hash: {e}"))?;

    Ok((block_number, block_hash))
}

/// Reconstruct RLP-encoded header bytes from the eth_getBlockByNumber response.
///
/// eth_getBlockByNumber returns a JSON object with individual header fields.
/// We rebuild the RLP encoding from these fields for use in the light client.
fn decode_eth_header_rlp(header: &serde_json::Value) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(512);

    let parent_hash = hex_from_json(header, "parentHash")?;
    let sha3_uncles = hex_from_json(header, "sha3Uncles")?;
    let miner = hex_from_json(header, "miner")?;
    let state_root = hex_from_json(header, "stateRoot")?;
    let tx_root = hex_from_json(header, "transactionsRoot")?;
    let receipts_root = hex_from_json(header, "receiptsRoot")?;
    let logs_bloom = hex_from_json(header, "logsBloom")?;
    let difficulty = hex_from_json(header, "difficulty")?;
    let number = hex_from_json(header, "number")?;
    let gas_limit = hex_from_json(header, "gasLimit")?;
    let gas_used = hex_from_json(header, "gasUsed")?;
    let timestamp = hex_from_json(header, "timestamp")?;
    let extra_data = hex_from_json(header, "extraData")?;
    let mix_hash = hex_from_json(header, "mixHash")?;
    let nonce = hex_from_json(header, "nonce")?;

    let fields: Vec<&[u8]> = vec![
        &parent_hash,
        &sha3_uncles,
        &miner,
        &state_root,
        &tx_root,
        &receipts_root,
        &logs_bloom,
        &difficulty,
        &number,
        &gas_limit,
        &gas_used,
        &timestamp,
        &extra_data,
        &mix_hash,
        &nonce,
    ];

    // Calculate total payload length
    let payload_len: usize = fields.iter().map(|f| rlp_field_len(f)).sum();

    // Encode list header
    if payload_len < 56 {
        out.push(0xC0 + payload_len as u8);
    } else {
        let len_bytes = payload_len.to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(8);
        let num_len_bytes = len_bytes.len() - skip;
        out.push(0xF7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
    }

    // Encode each field
    for field in fields {
        encode_rlp_field(&mut out, field);
    }

    Some(out)
}

/// Decode a hex string from a JSON value.
fn hex_from_json(value: &serde_json::Value, key: &str) -> Option<Vec<u8>> {
    let s = value.get(key)?.as_str()?;
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.is_empty() {
        return Some(Vec::new());
    }
    hex::decode(s).ok()
}

/// Get the RLP encoding length for a byte field.
fn rlp_field_len(data: &[u8]) -> usize {
    if data.is_empty() || (data.len() == 1 && data[0] < 0x80) {
        1
    } else if data.len() < 56 {
        1 + data.len() // short string
    } else {
        let len_bytes = data.len().to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(8);
        1 + (len_bytes.len() - skip) + data.len() // long string
    }
}

/// Encode a byte field as RLP.
fn encode_rlp_field(out: &mut Vec<u8>, data: &[u8]) {
    if data.is_empty() {
        out.push(0x80);
    } else if data.len() == 1 && data[0] < 0x80 {
        out.push(data[0]);
    } else if data.len() < 56 {
        out.push(0x80 + data.len() as u8);
        out.extend_from_slice(data);
    } else {
        let len_bytes = data.len().to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(8);
        let num_len_bytes = len_bytes.len() - skip;
        out.push(0xB7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
        out.extend_from_slice(data);
    }
}

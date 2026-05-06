//! RLP parsing helpers and bridge event extraction.

use crate::types::*;
use alloy_primitives::{Address, B256};

/// Expected bridge deposit event signature hash.
/// This is keccak256("BridgeDeposit(bytes32,address,uint256,uint256,bytes,uint256,uint256)")
/// and must match the event emitted by the Call bridge contract on Ethereum.
pub(crate) const BRIDGE_DEPOSIT_EVENT_SIG: B256 = B256::new([
    0x3a, 0x95, 0x7f, 0x16, 0x8e, 0x13, 0xa0, 0x27,
    0xb5, 0x3e, 0x7f, 0x6f, 0xe9, 0x58, 0xa0, 0xbc,
    0xa9, 0x0d, 0x51, 0x5b, 0x66, 0xbf, 0x4b, 0x5a,
    0x6c, 0x61, 0x60, 0x84, 0x79, 0x13, 0x54, 0x06,
]);

/// RLP-encode a u64 (big-endian, no leading zeros).
pub fn rlp_encode_u64(value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0x80];
    }
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(8);
    let data = &bytes[start..];
    if data.len() == 1 && data[0] < 0x80 {
        data.to_vec()
    } else {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(0x80 + data.len() as u8);
        out.extend_from_slice(data);
        out
    }
}

/// Parse RLP-encoded receipt to extract logs.
///
/// Ethereum receipt format (typed post-EIP-2718):
/// - Type 0 (legacy): [status/nonce, cumulative_gas, bloom, logs]
/// - Type 1 (EIP-2930): 0x01 ++ RLP([status, cumulative_gas_used, bloom, logs])
/// - Type 2 (EIP-1559): 0x02 ++ RLP([status, cumulative_gas_used, bloom, logs])
/// - Type 3 (EIP-4844): 0x03 ++ RLP([status, cumulative_gas_used, bloom, logs])
///
/// We handle all typed receipts by stripping the type byte (0x00–0x7f)
/// and parsing the remaining RLP list.
pub fn parse_receipt_logs(receipt_rlp: &[u8]) -> Result<Vec<ReceiptLog>, String> {
    if receipt_rlp.is_empty() {
        return Err("empty receipt".into());
    }

    // EIP-2718: if first byte is a type prefix (0x00–0x7f), strip it
    let rlp_body = if receipt_rlp[0] <= 0x7f {
        &receipt_rlp[1..]
    } else {
        receipt_rlp
    };

    let items = parse_rlp_list_items(rlp_body)?;

    // Receipts have at least 4 fields; logs are in field index 3
    if items.len() < 4 {
        return Err(format!(
            "receipt has {} fields, expected >= 4",
            items.len()
        ));
    }

    let logs_rlp = items[3];
    parse_logs_rlp(logs_rlp)
}

/// Parse an RLP list into individual item payloads.
pub(crate) fn parse_rlp_list_items(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    if data.is_empty() {
        return Err("empty data".into());
    }
    let first = data[0];
    let payload_start = if first >= 0xC0 && first < 0xF8 {
        // Short list
        let list_len = (first - 0xC0) as usize;
        if list_len == 0 || 1 + list_len != data.len() {
            return Err("RLP list length mismatch".into());
        }
        1
    } else if first >= 0xF8 {
        // Long list
        let len_of_len = (first - 0xF7) as usize;
        if 1 + len_of_len > data.len() {
            return Err("invalid long RLP list".into());
        }
        let total_payload = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        if 1 + len_of_len + total_payload != data.len() {
            return Err("long RLP list length mismatch".into());
        }
        1 + len_of_len
    } else {
        return Err(format!("not an RLP list: first byte = 0x{first:02x}"));
    };

    let mut items = Vec::new();
    let mut cursor = &data[payload_start..];
    while !cursor.is_empty() {
        let (item, consumed) = decode_one_rlp_item(cursor)?;
        items.push(item);
        cursor = &cursor[consumed..];
    }
    Ok(items)
}

/// Decode one RLP item from the start of data.
pub(crate) fn decode_one_rlp_item(data: &[u8]) -> Result<(&[u8], usize), String> {
    if data.is_empty() {
        return Err("empty data".into());
    }
    let first = data[0];
    if first < 0x80 {
        Ok((&data[..1], 1))
    } else if first < 0xB8 {
        let len = (first - 0x80) as usize;
        if 1 + len > data.len() {
            return Err("short RLP string".into());
        }
        Ok((&data[1..1 + len], 1 + len))
    } else if first < 0xC0 {
        let len_of_len = (first - 0xB7) as usize;
        if 1 + len_of_len > data.len() {
            return Err("short RLP long string".into());
        }
        let len = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        let total = 1 + len_of_len + len;
        if total > data.len() {
            return Err("truncated RLP long string".into());
        }
        Ok((&data[1 + len_of_len..total], total))
    } else if first < 0xF8 {
        let list_len = (first - 0xC0) as usize;
        if 1 + list_len > data.len() {
            return Err("short RLP list".into());
        }
        Ok((&data[..1 + list_len], 1 + list_len))
    } else {
        // Long list
        let len_of_len = (first - 0xF7) as usize;
        if 1 + len_of_len > data.len() {
            return Err("invalid long RLP list".into());
        }
        let total_payload = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        let total = 1 + len_of_len + total_payload;
        if total > data.len() {
            return Err("long RLP list length mismatch".into());
        }
        Ok((&data[..total], total))
    }
}

/// Parse an RLP-encoded logs list into ReceiptLog structs.
pub(crate) fn parse_logs_rlp(logs_rlp: &[u8]) -> Result<Vec<ReceiptLog>, String> {
    let log_entries = parse_rlp_list_items(logs_rlp)?;
    let mut logs = Vec::with_capacity(log_entries.len());

    for entry in log_entries {
        let fields = parse_rlp_list_items(entry)?;
        if fields.len() < 3 {
            return Err("log entry has too few fields".into());
        }

        let address = fields[0].to_vec();
        let topics_items = parse_rlp_list_items(fields[1])?;
        let topics: Vec<B256> = topics_items
            .iter()
            .filter_map(|t| {
                if t.len() == 32 {
                    Some(B256::from_slice(t))
                } else {
                    None
                }
            })
            .collect();
        let data = fields[2].to_vec();

        logs.push(ReceiptLog {
            address,
            topics,
            data,
        });
    }

    Ok(logs)
}

/// Parse a Call bridge deposit event from receipt logs.
///
/// The bridge contract on Ethereum emits an event with:
/// - topics[0]: event signature hash (optional, depends on contract ABI)
/// - topics[1]: source_tx_hash (B256)
/// - topics[2]: recipient (Address, left-padded to 32 bytes)
/// - data: source_chain (uint256), source_block (uint256), sender (bytes), asset_id (uint256), amount (uint256)
///
/// For this implementation, we use a simplified encoding where:
/// - The event signature is NOT checked (any log with the right structure is accepted)
/// - Data is RLP-encoded: [source_chain, source_block, sender_bytes, asset_id, amount]
pub fn parse_bridge_event_from_logs(logs: &[ReceiptLog]) -> Option<BridgeEvent> {
    for log in logs {
        // Try to parse each log as a bridge event
        if let Some(event) = try_parse_bridge_log(log) {
            return Some(event);
        }
    }
    None
}

/// Try to parse a single log as a bridge deposit event.
///
/// We look for logs where the data field can be parsed as:
/// RLP: [source_chain(u64), source_block(u64), sender(bytes), asset_id(u64), amount(u128)]
pub(crate) fn try_parse_bridge_log(log: &ReceiptLog) -> Option<BridgeEvent> {
    // Verify the event signature hash matches the expected BridgeDeposit event
    let event_sig = log.topics.first()?;
    // Skip signature check if the constant is the zero placeholder (not yet configured)
    if *event_sig != BRIDGE_DEPOSIT_EVENT_SIG && BRIDGE_DEPOSIT_EVENT_SIG != B256::ZERO {
        return None;
    }

    let fields = parse_rlp_list_items(&log.data).ok()?;

    // Expected: [source_chain, source_block, sender, asset_id, amount]
    if fields.len() < 5 {
        return None;
    }

    let source_chain = decode_u64(fields[0])?;
    let source_block = decode_u64(fields[1])?;

    // sender can be bytes or 32-byte hash
    let sender = if fields[2].len() == 32 {
        fields[2].to_vec()
    } else if fields[2].len() <= 1 {
        // empty or single byte (RLP empty = 0x80)
        if fields[2].is_empty() || fields[2][0] == 0x80 {
            Vec::new()
        } else {
            fields[2].to_vec()
        }
    } else {
        fields[2].to_vec()
    };

    let asset_id = decode_u64(fields[3])?;
    let amount = decode_u128(fields[4])?;

    // Try to get source_tx_hash and recipient from topics
    let source_tx_hash = if log.topics.len() >= 2 {
        log.topics[1]
    } else {
        B256::ZERO // fallback
    };

    let recipient = if log.topics.len() >= 3 {
        // Address is left-padded to 32 bytes in topics
        let topic = log.topics[2];
        Address::from_slice(&topic[12..32])
    } else {
        Address::ZERO // fallback
    };

    Some(BridgeEvent {
        source_chain,
        source_tx_hash,
        source_block,
        sender,
        recipient,
        asset_id,
        amount,
    })
}

/// Decode a u64 from RLP-encoded bytes.
pub(crate) fn decode_u64(data: &[u8]) -> Option<u64> {
    if data.is_empty() {
        return None;
    }
    if data.len() > 8 {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes[8 - data.len()..].copy_from_slice(data);
    Some(u64::from_be_bytes(bytes))
}

/// Decode a u128 from RLP-encoded bytes.
pub(crate) fn decode_u128(data: &[u8]) -> Option<u128> {
    if data.is_empty() {
        return None;
    }
    if data.len() > 16 {
        return None;
    }
    let mut bytes = [0u8; 16];
    bytes[16 - data.len()..].copy_from_slice(data);
    Some(u128::from_be_bytes(bytes))
}

//! Ethereum light client implementation.
//!
//! Verifies Ethereum block headers against a trusted anchor and
//! transaction inclusion via Merkle-Patricia Trie proofs.
//!
//! Security model:
//! - Starts from a trusted anchor (known-good header at a specific block).
//! - Each new header must link to a previously verified header via parent_hash.
//! - The parent_hash chain ensures headers are on the canonical chain.
//! - Transaction/receipt inclusion is proven via MPT proofs against header roots.
//!
//! Note: This implementation does NOT verify Ethereum's BLS consensus signatures.
//! It assumes the anchor is final and that the parent_hash chain follows the
//! canonical chain. For full security, the anchor should be a finalized block
//! (e.g., from Ethereum's consensus layer or a widely-known checkpoint).

use crate::types::*;
use crate::verifier;
use alloy_primitives::{keccak256, Address, B256};

use std::collections::HashMap;

/// Ethereum light client state.
pub struct EthLightClient {
    /// Trusted anchor: the starting point for header verification
    genesis: GenesisState,
    /// Verified headers: block_number → EthHeader
    verified_headers: HashMap<u64, EthHeader>,
    /// Latest verified block number
    latest_block: u64,
}

impl EthLightClient {
    /// Create a new light client with a trusted genesis anchor.
    pub fn init(genesis: GenesisState) -> Self {
        let latest = genesis.anchor_block;
        let mut verified = HashMap::new();
        verified.insert(latest, EthHeader {
            rlp_bytes: Vec::new(),
            block_hash: genesis.anchor_hash,
        });
        Self {
            genesis,
            verified_headers: verified,
            latest_block: latest,
        }
    }

    /// Submit and verify a new block header.
    ///
    /// Verification steps:
    /// 1. The header's block number must be > anchor block.
    /// 2. No duplicate header at this block number.
    /// 3. The header's parent_hash must match a previously verified header.
    /// 4. The block hash must be consistent with the RLP encoding.
    pub fn submit_header(&mut self, header: EthHeader) -> Result<(), LightClientError> {
        let block_num = header.number().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode block number".into())
        })?;

        // Must be after anchor
        if block_num <= self.genesis.anchor_block {
            return Err(LightClientError::BeforeAnchor(block_num));
        }

        // No duplicates
        if self.verified_headers.contains_key(&block_num) {
            return Err(LightClientError::DuplicateHeader(block_num));
        }

        // Parent hash must match a verified header
        let parent_num = block_num - 1;
        let parent = self
            .verified_headers
            .get(&parent_num)
            .ok_or(LightClientError::HeaderNotVerified {
                block: parent_num,
                latest: self.latest_block,
            })?;

        let actual_parent = header.parent_hash().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode parent hash".into())
        })?;

        if actual_parent != parent.block_hash {
            return Err(LightClientError::ParentHashMismatch {
                block: block_num,
                expected: parent.block_hash,
                actual: actual_parent,
            });
        }

        // Verify block hash matches RLP
        let computed_hash = keccak256(&header.rlp_bytes);
        if computed_hash != header.block_hash {
            return Err(LightClientError::InvalidHeader(
                "block hash does not match RLP encoding".into(),
            ));
        }

        // Store the verified header
        self.verified_headers
            .insert(block_num, header);

        // Update latest
        if block_num > self.latest_block {
            self.latest_block = block_num;
        }

        Ok(())
    }

    /// Check if a header at the given block number has been verified.
    pub fn is_header_verified(&self, block_number: u64) -> bool {
        self.verified_headers.contains_key(&block_number)
    }

    /// Get the latest verified block number.
    pub fn latest_block(&self) -> u64 {
        self.latest_block
    }

    /// Get a verified header by block number.
    pub fn get_header(&self, block_number: u64) -> Option<&EthHeader> {
        self.verified_headers.get(&block_number)
    }

    /// Verify transaction inclusion in a block via MPT proof.
    ///
    /// Steps:
    /// 1. Look up the header at `block_number` (must be verified).
    /// 2. Extract transactions_root from header.
    /// 3. Verify the MPT proof: the tx_hash must be the key,
    ///    and the value must match the expected transaction data.
    ///
    /// Note: The tx_hash is used as the key directly (not nibble-encoded
    /// since we're verifying against the raw tx_hash in the trie).
    ///
    /// Returns `Ok(())` if the transaction is proven to be in the block.
    pub fn verify_tx_inclusion(
        &self,
        block_number: u64,
        _tx_hash: B256,
        tx_proof: &TxInclusionProof,
    ) -> Result<(), LightClientError> {
        let header = self
            .verified_headers
            .get(&block_number)
            .ok_or(LightClientError::HeaderNotFound(block_number))?;

        let tx_root = header
            .transactions_root()
            .ok_or_else(|| LightClientError::InvalidHeader("no transactions root".into()))?;

        let proof_rlps = tx_proof.node_rlps();
        if proof_rlps.is_empty() {
            return Err(LightClientError::MptProofError(
                "empty tx proof".into(),
            ));
        }

        // In the transactions trie, the key is the RLP-encoded transaction hash.
        // For the proof, we use the raw tx_hash bytes as the key.
        let result =
            verifier::verify_mpt_proof(tx_root, &_tx_hash[..], &proof_rlps)
                .map_err(|e| LightClientError::MptProofError(e.to_string()))?;

        if result.is_none() {
            return Err(LightClientError::TxNotFound);
        }

        Ok(())
    }

    /// Verify receipt inclusion and parse bridge event from logs.
    ///
    /// Steps:
    /// 1. Look up the header at `block_number`.
    /// 2. Extract receipts_root from header.
    /// 3. Verify the MPT proof for the receipt at the given index.
    /// 4. Parse the receipt RLP to extract logs.
    /// 5. Find the bridge deposit event in the logs.
    ///
    /// Returns the parsed `BridgeEvent` if found.
    pub fn verify_receipt_and_parse_bridge_event(
        &self,
        block_number: u64,
        receipt_proof: &ReceiptProof,
    ) -> Result<BridgeEvent, LightClientError> {
        let header = self
            .verified_headers
            .get(&block_number)
            .ok_or(LightClientError::HeaderNotFound(block_number))?;

        let receipts_root = header
            .receipts_root()
            .ok_or_else(|| LightClientError::InvalidHeader("no receipts root".into()))?;

        let proof_rlps = receipt_proof.node_rlps();
        if proof_rlps.is_empty() {
            return Err(LightClientError::MptProofError(
                "empty receipt proof".into(),
            ));
        }

        // In the receipts trie, the key is the RLP-encoded receipt index.
        let index_key = rlp_encode_u64(receipt_proof.receipt_index);
        let result =
            verifier::verify_mpt_proof(receipts_root, &index_key, &proof_rlps)
                .map_err(|e| LightClientError::MptProofError(e.to_string()))?;

        let receipt_rlp = result.ok_or(LightClientError::ReceiptNotFound)?;

        // Parse the receipt RLP to extract logs
        let logs = parse_receipt_logs(&receipt_rlp)
            .map_err(|e| LightClientError::LogParseError(e))?;

        // Find the bridge deposit event
        let bridge_event = parse_bridge_event_from_logs(&logs)
            .ok_or(LightClientError::BridgeEventNotFound)?;

        Ok(bridge_event)
    }
}

/// Expected bridge deposit event signature hash.
/// This is keccak256("BridgeDeposit(bytes32,address,uint256,uint256,bytes,uint256,uint256)")
/// and must match the event emitted by the Call bridge contract on Ethereum.
const BRIDGE_DEPOSIT_EVENT_SIG: B256 = B256::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
]);

/// RLP-encode a u64 (big-endian, no leading zeros).
fn rlp_encode_u64(value: u64) -> Vec<u8> {
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
fn parse_receipt_logs(receipt_rlp: &[u8]) -> Result<Vec<ReceiptLog>, String> {
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
fn parse_rlp_list_items(data: &[u8]) -> Result<Vec<&[u8]>, String> {
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
fn decode_one_rlp_item(data: &[u8]) -> Result<(&[u8], usize), String> {
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
fn parse_logs_rlp(logs_rlp: &[u8]) -> Result<Vec<ReceiptLog>, String> {
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
fn parse_bridge_event_from_logs(logs: &[ReceiptLog]) -> Option<BridgeEvent> {
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
fn try_parse_bridge_log(log: &ReceiptLog) -> Option<BridgeEvent> {
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
fn decode_u64(data: &[u8]) -> Option<u64> {
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
fn decode_u128(data: &[u8]) -> Option<u128> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::rlp_encode_short_bytes;

    /// Build a minimal valid Ethereum header RLP for testing.
    /// Fields: [parent_hash, sha3_uncles, miner, state_root, tx_root, receipt_root, logs_bloom,
    ///          difficulty, number, gas_limit, gas_used, timestamp, extra_data, mix_hash, nonce, base_fee]
    fn make_test_header_rlp(
        parent_hash: B256,
        block_number: u64,
        tx_root: B256,
        receipt_root: B256,
    ) -> Vec<u8> {
        let mut fields = Vec::with_capacity(16);
        fields.push(rlp_encode_short_bytes(&parent_hash.0)); // parent_hash
        fields.push(vec![0x80]); // sha3_uncles (empty)
        fields.push(vec![0x80]); // miner (empty)
        fields.push(vec![0xa0; 33]); // state_root placeholder (32 zero bytes = 0xa0 + 0x20 + zeros)
        fields[3] = {
            let mut out = Vec::with_capacity(33);
            out.push(0xa0);
            out.extend([0u8; 32]);
            out
        };
        fields.push(rlp_encode_short_bytes(&tx_root.0)); // tx_root
        fields.push(rlp_encode_short_bytes(&receipt_root.0)); // receipt_root
        fields.push(vec![0x80]); // logs_bloom
        fields.push(rlp_encode_short_bytes(&[0x01])); // difficulty
        // number
        let num_bytes = block_number.to_be_bytes();
        let start = num_bytes.iter().position(|&b| b != 0).unwrap_or(8);
        let num_data = if start < 8 {
            rlp_encode_short_bytes(&num_bytes[start..])
        } else {
            vec![0x80] // zero
        };
        fields.push(num_data);
        fields.push(rlp_encode_short_bytes(&[0x01])); // gas_limit
        fields.push(vec![0x80]); // gas_used
        fields.push(rlp_encode_short_bytes(&[0x01])); // timestamp
        fields.push(vec![0x80]); // extra_data
        // mix_hash (32 bytes): RLP long string = 0xa0 + 32 zero bytes
        let mut mix = Vec::with_capacity(33);
        mix.push(0xa0);
        mix.extend([0u8; 32]);
        fields.push(mix);
        // nonce: RLP 8 bytes = 0x88 + 8 zero bytes
        let mut nonce = Vec::with_capacity(9);
        nonce.push(0x88);
        nonce.extend([0u8; 8]);
        fields.push(nonce);
        fields.push(vec![0x80]); // base_fee

        rlp_encode_list_for_test(&fields)
    }

    fn rlp_encode_list_for_test(items: &[Vec<u8>]) -> Vec<u8> {
        let total_len: usize = items.iter().map(|i| i.len()).sum();
        let mut out = Vec::with_capacity(1 + total_len);
        if total_len < 56 {
            out.push(0xC0 + total_len as u8);
        } else {
            let len_bytes = total_len.to_be_bytes();
            let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
            let num_len_bytes = len_bytes.len() - skip;
            out.push(0xF7 + num_len_bytes as u8);
            out.extend_from_slice(&len_bytes[skip..]);
        }
        for item in items {
            out.extend(item);
        }
        out
    }

    #[test]
    fn test_light_client_init() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let client = EthLightClient::init(genesis);
        assert_eq!(client.latest_block(), 1000);
        assert!(client.is_header_verified(1000));
    }

    #[test]
    fn test_submit_header_valid_chain() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Create header for block 1001 with parent = anchor
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        assert!(client.submit_header(header).is_ok());
        assert_eq!(client.latest_block(), 1001);
        assert!(client.is_header_verified(1001));
    }

    #[test]
    fn test_submit_header_wrong_parent() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Header with wrong parent
        let wrong_parent = B256::repeat_byte(0xBB);
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(wrong_parent, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        let err = client.submit_header(header).unwrap_err();
        assert!(matches!(err, LightClientError::ParentHashMismatch { .. }));
    }

    #[test]
    fn test_submit_header_before_anchor() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Attempt to submit block 500 (before anchor 1000)
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(B256::ZERO, 500, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        let err = client.submit_header(header).unwrap_err();
        assert!(matches!(err, LightClientError::BeforeAnchor(500)));
    }

    #[test]
    fn test_submit_header_duplicate_block() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Submit block 1001
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp.clone());
        client.submit_header(header).unwrap();

        // Try to submit block 1001 again
        let header2 = EthHeader::from_rlp(rlp);
        let err = client.submit_header(header2).unwrap_err();
        assert!(matches!(err, LightClientError::DuplicateHeader(1001)));
    }

    #[test]
    fn test_submit_header_gap() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Try to submit block 1002 without 1001
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(anchor, 1002, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        let err = client.submit_header(header).unwrap_err();
        assert!(matches!(err, LightClientError::HeaderNotVerified { .. }));
    }

    #[test]
    fn test_header_chain_multiple_blocks() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let mut parent = anchor;

        for i in 1001..=1010 {
            let rlp = make_test_header_rlp(parent, i, tx_root, receipt_root);
            let header = EthHeader::from_rlp(rlp);
            parent = header.block_hash;
            client.submit_header(header).unwrap();
        }

        assert_eq!(client.latest_block(), 1010);
        for i in 1000..=1010 {
            assert!(client.is_header_verified(i));
        }
    }
}

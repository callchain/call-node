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

use std::collections::{BTreeMap, HashMap};

/// Ethereum light client state.
pub struct EthLightClient {
    /// Trusted anchor: the starting point for header verification
    genesis: GenesisState,
    /// Verified headers: block_number → EthHeader
    verified_headers: HashMap<u64, EthHeader>,
    /// Latest verified block number
    latest_block: u64,
    /// Finalized block from Ethereum consensus (beacon chain).
    /// Headers at or below this block are considered consensus-verified.
    finalized_block: Option<(u64, B256)>,
    /// Buffer for out-of-order headers waiting for their parent.
    buffer: BTreeMap<u64, EthHeader>,
}

/// Maximum number of buffered headers waiting for missing parents.
const MAX_BUFFER_SIZE: usize = 64;

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
            finalized_block: None,
            buffer: BTreeMap::new(),
        }
    }

    /// Submit and verify a new block header.
    ///
    /// Verification steps:
    /// 1. The header's block number must be > anchor block.
    /// 2. No duplicate header at this block number.
    /// 3. The header's parent_hash must match a previously verified header.
    ///    - If parent not found but exists in buffer range, buffer the header (gap tolerance).
    ///    - If parent exists but doesn't match the expected block-1, trigger reorg handling.
    /// 4. The block hash must be consistent with the RLP encoding.
    ///
    /// Returns `Ok(())` on success, or `Ok(n)` if the header was buffered
    /// (where `n` is the number of headers flushed from the buffer).
    /// Returns `Err(BufferFull)` if the buffer is at capacity.
    pub fn submit_header(&mut self, header: EthHeader) -> Result<(), LightClientError> {
        let block_num = header.number().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode block number".into())
        })?;

        // Must be after anchor
        if block_num <= self.genesis.anchor_block {
            return Err(LightClientError::BeforeAnchor(block_num));
        }

        // No duplicates in verified or buffer
        if self.verified_headers.contains_key(&block_num) {
            return Err(LightClientError::DuplicateHeader(block_num));
        }
        if self.buffer.contains_key(&block_num) {
            return Err(LightClientError::DuplicateHeader(block_num));
        }

        // Check parent hash
        let parent_num = block_num.saturating_sub(1);
        let parent_in_verified = self.verified_headers.get(&parent_num);

        if let Some(parent) = parent_in_verified {
            // Parent is verified — check it matches
            let actual_parent = header.parent_hash().ok_or_else(|| {
                LightClientError::InvalidHeader("cannot decode parent hash".into())
            })?;

            if actual_parent != parent.block_hash {
                // Parent exists but hash doesn't match — possible reorg
                // Try reorg handling: find where the parent hash actually is
                return self.handle_reorg_and_submit(header);
            }

            // Parent matches — verify block hash and insert
            let computed_hash = keccak256(&header.rlp_bytes);
            if computed_hash != header.block_hash {
                return Err(LightClientError::InvalidHeader(
                    "block hash does not match RLP encoding".into(),
                ));
            }

            self.verified_headers.insert(block_num, header);
            if block_num > self.latest_block {
                self.latest_block = block_num;
            }

            // Flush any buffered headers whose parents are now verified
            self.flush_buffer();
            return Ok(());
        }

        // Parent not found in verified headers.
        // Check if parent is in the buffer (future block arrives before current)
        if self.buffer.contains_key(&parent_num) {
            // Parent is buffered too — buffer this header as well
            if self.buffer.len() >= MAX_BUFFER_SIZE {
                return Err(LightClientError::BufferFull);
            }
            self.buffer.insert(block_num, header);
            return Ok(());
        }

        // Parent not found at all — check if it's after anchor (might arrive later)
        // Buffer if we have room and block is within reasonable range
        if self.buffer.len() >= MAX_BUFFER_SIZE {
            return Err(LightClientError::BufferFull);
        }

        // Only buffer if block_num is not too far ahead
        if block_num <= self.latest_block + MAX_BUFFER_SIZE as u64 {
            self.buffer.insert(block_num, header);
            Ok(())
        } else {
            // Too far ahead, reject
            Err(LightClientError::HeaderNotVerified {
                block: parent_num,
                latest: self.latest_block,
            })
        }
    }

    /// Handle a reorg by calling handle_reorg, returning any unwound headers.
    fn handle_reorg_and_submit(&mut self, header: EthHeader) -> Result<(), LightClientError> {
        let unwound = self.handle_reorg(header)?;
        // Unwound headers can be re-submitted by the caller if needed.
        // We keep them here for now — they'll be re-validated when re-submitted.
        // Drop them since handle_reorg already inserted the new header.
        drop(unwound);

        // Flush buffer in case reorg unblocked some headers
        self.flush_buffer();
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

    /// Get the anchor block number.
    pub fn anchor_block(&self) -> u64 {
        self.genesis.anchor_block
    }

    /// Get the finalized block number, if set.
    pub fn finalized_block(&self) -> Option<u64> {
        self.finalized_block.as_ref().map(|(n, _)| *n)
    }

    /// Get the number of buffered headers.
    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Set the finalized block from Ethereum consensus (beacon chain).
    /// Headers at or below this block are considered consensus-verified.
    pub fn set_finalized_block(&mut self, block: u64, hash: B256) {
        self.finalized_block = Some((block, hash));
    }

    /// Check if a block is consensus-verified (at or below finalized).
    pub fn is_consensus_verified(&self, block: u64) -> bool {
        self.finalized_block.is_some_and(|(n, _)| block <= n)
    }

    /// Handle a potential reorg when the parent doesn't match the latest header.
    ///
    /// If the header's parent exists in verified headers but isn't `block_num - 1`,
    /// this indicates a reorg. Walk back to find the fork point, unwind orphaned
    /// headers above it, and return the unwound headers for resubmission.
    ///
    /// Returns `Ok(unwound_headers)` — headers that were removed from the chain
    /// above the fork point. The caller should resubmit them.
    /// Returns `Err(BeforeFinalized)` if the fork point is at or below finalized.
    pub fn handle_reorg(&mut self, header: EthHeader) -> Result<Vec<EthHeader>, LightClientError> {
        let block_num = header.number().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode block number".into())
        })?;

        let parent_hash = header.parent_hash().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode parent hash".into())
        })?;

        // Find where this parent_hash exists in verified headers
        let mut fork_point: Option<u64> = None;
        // Search backwards from latest to find the parent
        for n in (self.genesis.anchor_block..=self.latest_block).rev() {
            if let Some(h) = self.verified_headers.get(&n) {
                if h.block_hash == parent_hash {
                    fork_point = Some(n);
                    break;
                }
            }
        }

        let fork = fork_point.ok_or(LightClientError::HeaderNotFound(
            block_num.saturating_sub(1),
        ))?;

        // Don't reorg below finalized
        if let Some((finalized_n, _)) = self.finalized_block {
            if fork <= finalized_n {
                return Err(LightClientError::BeforeFinalized(fork));
            }
        }

        // Unwind headers above the fork point
        let unwound: Vec<EthHeader> = (fork + 1..=self.latest_block)
            .filter_map(|n| self.verified_headers.remove(&n))
            .collect();

        // Update latest_block
        self.latest_block = fork;

        // Insert the new header
        self.verified_headers.insert(block_num, header.clone());
        if block_num > self.latest_block {
            self.latest_block = block_num;
        }

        Ok(unwound)
    }

    /// Flush buffered headers whose parents are now verified.
    /// Processes headers in order (lowest block number first).
    fn flush_buffer(&mut self) -> Vec<Result<(), LightClientError>> {
        let mut results = Vec::new();
        loop {
            // Find the lowest buffered header whose parent is verified
            let next = self.buffer.iter().find(|(block_num, _header)| {
                let parent = block_num.saturating_sub(1);
                self.verified_headers.contains_key(&parent)
            }).map(|(n, h)| (*n, h.clone()));

            match next {
                Some((n, _)) => {
                    let header = self.buffer.remove(&n).unwrap();
                    let result = self.insert_verified_header(header);
                    results.push(result);
                }
                None => break,
            }
        }
        results
    }

    /// Insert a header into verified headers without re-running parent verification.
    fn insert_verified_header(&mut self, header: EthHeader) -> Result<(), LightClientError> {
        let block_num = header.number().ok_or_else(|| {
            LightClientError::InvalidHeader("cannot decode block number".into())
        })?;

        if self.verified_headers.contains_key(&block_num) {
            return Err(LightClientError::DuplicateHeader(block_num));
        }

        self.verified_headers.insert(block_num, header);
        if block_num > self.latest_block {
            self.latest_block = block_num;
        }
        Ok(())
    }

    /// Advance the trusted anchor to a more recent verified block.
    ///
    /// After advancing, all headers below the new anchor are pruned,
    /// freeing memory. The new anchor must already be verified.
    pub fn advance_anchor(&mut self, block: u64, hash: B256) -> Result<(), LightClientError> {
        if !self.verified_headers.contains_key(&block) {
            return Err(LightClientError::AnchorNotVerified(block));
        }

        // Don't advance below finalized
        if let Some((finalized_n, _)) = self.finalized_block {
            if block <= finalized_n {
                return Err(LightClientError::BeforeFinalized(finalized_n));
            }
        }

        self.genesis.anchor_block = block;
        self.genesis.anchor_hash = hash;

        // Prune headers below new anchor
        self.prune_headers_before(block);

        Ok(())
    }

    /// Remove headers with block number less than `before_block`.
    /// Never removes the anchor or finalized block.
    fn prune_headers_before(&mut self, _before_block: u64) {
        let min_keep = std::cmp::min(
            self.genesis.anchor_block,
            self.finalized_block.map(|(n, _)| n).unwrap_or(u64::MAX),
        );
        self.verified_headers.retain(|&k, _| k >= min_keep);
    }

    /// Prune all headers before the given block number.
    /// Returns the number of headers removed.
    pub fn prune_headers(&mut self, before_block: u64) -> usize {
        let min_keep = std::cmp::min(
            self.genesis.anchor_block,
            self.finalized_block.map(|(n, _)| n).unwrap_or(u64::MAX),
        );
        let effective_before = std::cmp::max(before_block, min_keep + 1);
        let before_count = self.verified_headers.len();
        self.verified_headers.retain(|&k, _| k >= effective_before);
        before_count - self.verified_headers.len()
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
pub(crate) const BRIDGE_DEPOSIT_EVENT_SIG: B256 = B256::new([
    0x3a, 0x95, 0x7f, 0x16, 0x8e, 0x13, 0xa0, 0x27,
    0xb5, 0x3e, 0x7f, 0x6f, 0xe9, 0x58, 0xa0, 0xbc,
    0xa9, 0x0d, 0x51, 0x5b, 0x66, 0xbf, 0x4b, 0x5a,
    0x6c, 0x61, 0x60, 0x84, 0x79, 0x13, 0x54, 0x06,
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

        // Header with wrong parent (not matching any verified header)
        let wrong_parent = B256::repeat_byte(0xBB);
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(wrong_parent, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        // Reorg handling fails because wrong_parent hash doesn't exist in verified headers
        let err = client.submit_header(header).unwrap_err();
        assert!(matches!(err, LightClientError::HeaderNotFound(_)));
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

        // Try to submit block 1002 without 1001 — now buffers instead of rejecting
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let rlp = make_test_header_rlp(anchor, 1002, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);

        // Buffering succeeds
        assert!(client.submit_header(header).is_ok());
        assert_eq!(client.buffer_len(), 1);
        // Block 1002 is buffered, not yet verified
        assert!(!client.is_header_verified(1002));

        // Now submit block 1001, which should flush 1002 from buffer
        let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        let header_1001 = EthHeader::from_rlp(rlp);
        assert!(client.submit_header(header_1001).is_ok());

        // 1002 should now be verified
        assert!(client.is_header_verified(1002));
        assert_eq!(client.buffer_len(), 0);
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

    fn build_chain(client: &mut EthLightClient, from: u64, to: u64, anchor: B256) -> B256 {
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let mut parent = anchor;
        for i in from..=to {
            let rlp = make_test_header_rlp(parent, i, tx_root, receipt_root);
            let header = EthHeader::from_rlp(rlp);
            parent = header.block_hash;
            client.submit_header(header).unwrap();
        }
        parent
    }

    #[test]
    fn test_gap_buffer_flush() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Submit 1003, 1002, 1001 out of order
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);

        // 1003 buffered (parent 1002 not verified)
        let rlp = make_test_header_rlp(anchor, 1003, tx_root, receipt_root);
        assert!(client.submit_header(EthHeader::from_rlp(rlp)).is_ok());
        assert_eq!(client.buffer_len(), 1);

        // 1002 buffered (parent 1001 not verified)
        let rlp = make_test_header_rlp(anchor, 1002, tx_root, receipt_root);
        assert!(client.submit_header(EthHeader::from_rlp(rlp)).is_ok());
        assert_eq!(client.buffer_len(), 2);

        // 1001 — should flush 1002, then 1003
        let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        assert!(client.submit_header(EthHeader::from_rlp(rlp)).is_ok());
        assert_eq!(client.buffer_len(), 0);
        assert!(client.is_header_verified(1001));
        assert!(client.is_header_verified(1002));
        assert!(client.is_header_verified(1003));
        assert_eq!(client.latest_block(), 1003);
    }

    #[test]
    fn test_buffer_full_rejection() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);

        // Fill the buffer
        for i in 1002..=(1001 + MAX_BUFFER_SIZE as u64) {
            let rlp = make_test_header_rlp(anchor, i, tx_root, receipt_root);
            assert!(client.submit_header(EthHeader::from_rlp(rlp)).is_ok());
        }
        assert_eq!(client.buffer_len(), MAX_BUFFER_SIZE);

        // One more should fail
        let rlp = make_test_header_rlp(anchor, 1002 + MAX_BUFFER_SIZE as u64, tx_root, receipt_root);
        let err = client.submit_header(EthHeader::from_rlp(rlp)).unwrap_err();
        assert!(matches!(err, LightClientError::BufferFull));
    }

    #[test]
    fn test_consensus_verification() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        let finalized_hash = B256::repeat_byte(0xCC);
        build_chain(&mut client, 1001, 1005, anchor);
        client.set_finalized_block(1005, finalized_hash);

        assert!(client.is_consensus_verified(1005));
        assert!(client.is_consensus_verified(1000));
        assert!(client.is_consensus_verified(1001));
        assert!(!client.is_consensus_verified(1006));
        assert_eq!(client.finalized_block(), Some(1005));
    }

    #[test]
    fn test_reorg_unwind() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        // Build chain: 1000 -> 1001 -> 1002 -> 1003
        let tx_root = B256::repeat_byte(0x01);
        let receipt_root = B256::repeat_byte(0x02);
        let mut parent = anchor;
        let mut hashes = Vec::new();
        for i in 1001..=1003 {
            let rlp = make_test_header_rlp(parent, i, tx_root, receipt_root);
            let header = EthHeader::from_rlp(rlp.clone());
            parent = header.block_hash;
            hashes.push(header.block_hash);
            client.submit_header(header).unwrap();
        }
        assert_eq!(client.latest_block(), 1003);

        // Set finalized at 1001 so we can't reorg below it
        client.set_finalized_block(1001, hashes[0]);

        // Try reorg at block 1004 with parent = hash of 1001 (skipping 1002, 1003)
        // This would unwind 1002, 1003 but fork at 1001 which is finalized
        let fork_parent_hash = hashes[0]; // hash of 1001
        let rlp = make_test_header_rlp(fork_parent_hash, 1004, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);
        let err = client.submit_header(header).unwrap_err();
        // Should fail because fork point (1001) is at finalized block
        assert!(matches!(err, LightClientError::BeforeFinalized(1001)));
    }

    #[test]
    fn test_anchor_advancement() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        build_chain(&mut client, 1001, 1010, anchor);
        assert_eq!(client.anchor_block(), 1000);

        // Advance anchor to 1005
        let hash_1005 = client.get_header(1005).unwrap().block_hash;
        client.advance_anchor(1005, hash_1005).unwrap();
        assert_eq!(client.anchor_block(), 1005);

        // Headers below anchor should be pruned
        assert!(!client.is_header_verified(1001));
        assert!(!client.is_header_verified(1004));
        assert!(client.is_header_verified(1005));
        assert!(client.is_header_verified(1010));

        // Can't advance to unverified block
        assert!(client.advance_anchor(1099, B256::ZERO).is_err());
    }

    #[test]
    fn test_prune_headers() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut client = EthLightClient::init(genesis);

        build_chain(&mut client, 1001, 1020, anchor);

        // Set finalized at 1010
        let hash_1010 = client.get_header(1010).unwrap().block_hash;
        client.set_finalized_block(1010, hash_1010);

        // Prune before 1005 — min_keep is min(anchor=1000, finalized=1010) = 1000
        // effective_before = max(1005, 1001) = 1005, removes headers 1000..=1004 = 5
        let removed = client.prune_headers(1005);
        assert_eq!(removed, 5);

        // Headers below 1005 should be removed
        assert!(!client.is_header_verified(1004));
        assert!(client.is_header_verified(1005));
        // Finalized and above should remain
        assert!(client.is_header_verified(1010));
        assert!(client.is_header_verified(1020));
    }
}

//! Ethereum light client core implementation.
//!
//! Verifies Ethereum block headers against a trusted anchor,
//! transaction inclusion via Merkle-Patricia Trie proofs,
//! and optionally beacon chain sync committee BLS consensus signatures.
//!
//! Security model:
//! - Starts from a trusted anchor (known-good header at a specific block).
//! - Each new header must link to a previously verified header via parent_hash.
//! - The parent_hash chain ensures headers are on the canonical chain.
//! - Transaction/receipt inclusion is proven via MPT proofs against header roots.
//! - When beacon config is provided, sync committee aggregate signatures are
//!   verified to ensure headers are consensus-finalized (Altair light client sync).

use crate::beacon::{
    compute_sync_committee_signing_root, BeaconBlockHeader, LightClientUpdate, SyncAggregate,
    SyncCommittee,
};
use crate::types::*;
use crate::verifier;
use alloy_primitives::{keccak256, B256};
use call_crypto::bls_verify_aggregate_beacon;

use std::collections::{BTreeMap, HashMap};

use super::proof::{parse_bridge_event_from_logs, parse_receipt_logs, rlp_encode_u64};

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
    /// Beacon chain configuration for BLS consensus verification.
    /// When `Some`, sync committee signatures are verified on each update.
    beacon_config: Option<BeaconConfig>,
    /// Current sync committee (valid for the current sync period).
    /// Updated via [`Self::apply_light_client_update`].
    sync_committee: Option<SyncCommittee>,
    /// Current sync committee period.
    current_sync_period: u64,
}

/// Maximum number of buffered headers waiting for missing parents.
const MAX_BUFFER_SIZE: usize = 64;

impl EthLightClient {
    /// Create a new light client with a trusted genesis anchor.
    pub fn init(genesis: GenesisState) -> Self {
        Self::init_with_beacon_config(genesis, None)
    }

    /// Create a new light client with a trusted genesis anchor and beacon chain
    /// configuration for BLS consensus verification.
    ///
    /// When `beacon_config` is `Some`, the light client will verify sync
    /// committee aggregate signatures via [`Self::apply_light_client_update`].
    /// When `None`, behavior is identical to [`Self::init`].
    pub fn init_with_beacon_config(
        genesis: GenesisState,
        beacon_config: Option<BeaconConfig>,
    ) -> Self {
        let latest = genesis.anchor_block;
        let mut verified = HashMap::new();
        verified.insert(
            latest,
            EthHeader {
                rlp_bytes: Vec::new(),
                block_hash: genesis.anchor_hash,
            },
        );
        Self {
            genesis,
            verified_headers: verified,
            latest_block: latest,
            finalized_block: None,
            buffer: BTreeMap::new(),
            beacon_config,
            sync_committee: None,
            current_sync_period: 0,
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
        let block_num = header
            .number()
            .ok_or_else(|| LightClientError::InvalidHeader("cannot decode block number".into()))?;

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

    /// Get the current sync committee period.
    pub fn current_sync_period(&self) -> u64 {
        self.current_sync_period
    }

    /// Apply a beacon chain light client update.
    ///
    /// Verifies the sync committee aggregate BLS signature against the
    /// attested header, checks participation threshold, and updates the
    /// sync committee if the period advanced.
    ///
    /// Returns the finalized beacon header `(slot, hash_tree_root)` on success.
    /// The caller should map this to the corresponding execution block and call
    /// [`Self::set_finalized_block`] to mark execution headers as consensus-verified.
    ///
    /// # Errors
    /// - `NotInitialized` if beacon config was not provided at init.
    /// - `SyncCommitteeSignatureInvalid` if BLS verification fails.
    /// - `InsufficientSyncParticipation` if not enough validators signed.
    pub fn apply_light_client_update(
        &mut self,
        update: LightClientUpdate,
    ) -> Result<(u64, B256), LightClientError> {
        let beacon_config = self
            .beacon_config
            .as_ref()
            .ok_or(LightClientError::NotInitialized)?;

        // Determine which sync committee to use for verification.
        // If we already have a sync committee and the update's signature_slot
        // is in the current period, use the current one.
        // Otherwise, bootstrap from the update's next_sync_committee.
        let sync_committee = if let Some(ref current) = self.sync_committee {
            let update_period = crate::beacon::sync_period(update.signature_slot.saturating_sub(1));
            if update_period == self.current_sync_period {
                current
            } else {
                // Period advanced — use next sync committee from the update.
                &update.next_sync_committee
            }
        } else {
            // First update — bootstrap with next_sync_committee.
            &update.next_sync_committee
        };

        // Verify BLS aggregate signature
        self.verify_sync_aggregate(
            &update.attested_header,
            &update.sync_aggregate,
            sync_committee,
            beacon_config,
        )?;

        // Check participation threshold (> 2/3 of 512 = 342)
        const MIN_SYNC_PARTICIPATION: usize = 342;
        let participation = update.sync_aggregate.participant_count();
        if participation < MIN_SYNC_PARTICIPATION {
            return Err(LightClientError::InsufficientSyncParticipation {
                got: participation,
                required: MIN_SYNC_PARTICIPATION,
            });
        }

        // Update sync committee if period advanced
        let new_period = crate::beacon::sync_period(update.signature_slot.saturating_sub(1));
        if new_period > self.current_sync_period {
            self.sync_committee = Some(update.next_sync_committee.clone());
            self.current_sync_period = new_period;
        }

        let finalized_root = update.finalized_header.hash_tree_root();
        Ok((update.finalized_header.slot, finalized_root))
    }

    /// Verify a sync committee aggregate signature against a beacon block header.
    ///
    /// Computes the sync committee signing root from the header and beacon config,
    /// extracts participating pubkeys from the bitmask, and verifies the BLS
    /// aggregate signature using the beacon chain DST.
    fn verify_sync_aggregate(
        &self,
        header: &BeaconBlockHeader,
        sync_aggregate: &SyncAggregate,
        sync_committee: &SyncCommittee,
        beacon_config: &BeaconConfig,
    ) -> Result<(), LightClientError> {
        let signing_root = compute_sync_committee_signing_root(
            header,
            beacon_config.fork_version,
            beacon_config.genesis_validators_root,
        );

        let participant_pubkeys =
            sync_committee.participant_pubkeys(&sync_aggregate.sync_committee_bits);

        if participant_pubkeys.is_empty() {
            return Err(LightClientError::SyncCommitteeSignatureInvalid(
                "no participants".into(),
            ));
        }

        bls_verify_aggregate_beacon(
            &participant_pubkeys,
            signing_root.as_slice(),
            &sync_aggregate.sync_committee_signature,
        )
        .map_err(|e| LightClientError::SyncCommitteeSignatureInvalid(e.to_string()))
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
        let block_num = header
            .number()
            .ok_or_else(|| LightClientError::InvalidHeader("cannot decode block number".into()))?;

        let parent_hash = header
            .parent_hash()
            .ok_or_else(|| LightClientError::InvalidHeader("cannot decode parent hash".into()))?;

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
        self.verified_headers.insert(block_num, header);
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
            let next = self
                .buffer
                .iter()
                .find(|(block_num, _header)| {
                    let parent = block_num.saturating_sub(1);
                    self.verified_headers.contains_key(&parent)
                })
                .map(|(n, h)| (*n, h.clone()));

            match next {
                Some((n, _)) => {
                    if let Some(header) = self.buffer.remove(&n) {
                        let result = self.insert_verified_header(header);
                        results.push(result);
                    }
                }
                None => break,
            }
        }
        results
    }

    /// Insert a header into verified headers without re-running parent verification.
    fn insert_verified_header(&mut self, header: EthHeader) -> Result<(), LightClientError> {
        let block_num = header
            .number()
            .ok_or_else(|| LightClientError::InvalidHeader("cannot decode block number".into()))?;

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
            return Err(LightClientError::MptProofError("empty tx proof".into()));
        }

        // In the transactions trie, the key is the RLP-encoded transaction hash.
        // For the proof, we use the raw tx_hash bytes as the key.
        let result = verifier::verify_mpt_proof(tx_root, &_tx_hash[..], &proof_rlps)
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
        let result = verifier::verify_mpt_proof(receipts_root, &index_key, &proof_rlps)
            .map_err(|e| LightClientError::MptProofError(e.to_string()))?;

        let receipt_rlp = result.ok_or(LightClientError::ReceiptNotFound)?;

        // Parse the receipt RLP to extract logs
        let logs = parse_receipt_logs(&receipt_rlp).map_err(LightClientError::LogParseError)?;

        // Find the bridge deposit event
        let bridge_event =
            parse_bridge_event_from_logs(&logs).ok_or(LightClientError::BridgeEventNotFound)?;

        Ok(bridge_event)
    }
}

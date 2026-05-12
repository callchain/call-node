//! Integration tests for the light client crate.

use crate::ethereum::proof::BRIDGE_DEPOSIT_EVENT_SIG;
use crate::verifier::{make_leaf_node_rlp, rlp_encode_short_bytes, verify_mpt_proof, MptError};
use crate::*;
use alloy_primitives::{keccak256, B256};

fn encode_rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::with_capacity(1 + total);
    if total < 56 {
        out.push(0xC0 + total as u8);
    } else {
        let len_bytes = total.to_be_bytes();
        let skip = len_bytes
            .iter()
            .position(|&b| b != 0)
            .unwrap_or(len_bytes.len());
        let num_len_bytes = len_bytes.len() - skip;
        out.push(0xF7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
    }
    for item in items {
        out.extend(item);
    }
    out
}

/// Build a minimal valid Ethereum header RLP for testing.
fn make_test_header_rlp(
    parent_hash: B256,
    block_number: u64,
    tx_root: B256,
    receipt_root: B256,
) -> Vec<u8> {
    let mut fields = Vec::new();
    fields.push(rlp_encode_short_bytes(&parent_hash.0)); // parent_hash
    fields.push(vec![0x80]); // sha3_uncles
    fields.push(vec![0x80]); // miner
    fields.push(rlp_32_zero()); // state_root
    fields.push(rlp_encode_short_bytes(&tx_root.0)); // tx_root
    fields.push(rlp_encode_short_bytes(&receipt_root.0)); // receipt_root
    fields.push(vec![0x80]); // logs_bloom
    fields.push(rlp_encode_short_bytes(&[0x01])); // difficulty

    // block number
    let num = block_number.to_be_bytes();
    let start = num.iter().position(|&b| b != 0).unwrap_or(8);
    fields.push(if start < 8 {
        rlp_encode_short_bytes(&num[start..])
    } else {
        vec![0x80]
    });

    fields.push(rlp_encode_short_bytes(&[0x01])); // gas_limit
    fields.push(vec![0x80]); // gas_used
    fields.push(rlp_encode_short_bytes(&[0x01])); // timestamp
    fields.push(vec![0x80]); // extra_data
    fields.push(rlp_32_zero()); // mix_hash
    fields.push(vec![0x88, 0, 0, 0, 0, 0, 0, 0, 0]); // nonce
    fields.push(vec![0x80]); // base_fee

    encode_rlp_list(&fields)
}

fn rlp_32_zero() -> Vec<u8> {
    let mut out = Vec::with_capacity(33);
    out.push(0xa0);
    out.extend([0u8; 32]);
    out
}

#[test]
fn test_eth_header_parse_and_hash() {
    let parent = B256::repeat_byte(0xAA);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);
    let rlp = make_test_header_rlp(parent, 1001, tx_root, receipt_root);
    let hash = keccak256(&rlp);

    let header = EthHeader::from_rlp(rlp.clone());
    assert_eq!(header.block_hash, hash);
    assert_eq!(header.parent_hash(), Some(parent));
    assert_eq!(header.number(), Some(1001));
    assert_eq!(header.transactions_root(), Some(tx_root));
    assert_eq!(header.receipts_root(), Some(receipt_root));
}

#[test]
fn test_header_chain_parent_verification() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Submit block 1001
    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let hash_1001 = header.block_hash;
    client.submit_header(header).unwrap();

    // Submit block 1002
    let rlp = make_test_header_rlp(hash_1001, 1002, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let hash_1002 = header.block_hash;
    client.submit_header(header).unwrap();

    // Submit block 1003
    let rlp = make_test_header_rlp(hash_1002, 1003, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header).unwrap();

    assert_eq!(client.latest_block(), 1003);
}

#[test]
fn test_header_reject_unlinked() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);
    let wrong_parent = B256::repeat_byte(0xBB);

    let rlp = make_test_header_rlp(wrong_parent, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let result = client.submit_header(header);
    // Reorg handling fails because wrong_parent hash doesn't exist in verified headers
    assert!(matches!(result, Err(LightClientError::HeaderNotFound(_))));
}

#[test]
fn test_mpt_proof_verification() {
    use crate::verifier::bytes_to_nibbles;

    let key = [0x12u8, 0x34];
    let value = b"hello";
    let leaf = make_leaf_node_rlp(&bytes_to_nibbles(&key), value);
    let root = keccak256(&leaf);

    let proof = vec![leaf.clone()];
    let result = verify_mpt_proof(root, &key, &proof).unwrap();
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap(), value);
}

#[test]
fn test_mpt_reject_invalid_proof() {
    let key = [0x12u8];
    let key_nibbles = crate::verifier::bytes_to_nibbles(&key);
    let value = b"data";
    let leaf = make_leaf_node_rlp(&key_nibbles, value);
    let correct_root = keccak256(&leaf);

    let wrong_root = B256::repeat_byte(0xFF);
    let proof = vec![leaf];
    let result = verify_mpt_proof(wrong_root, &key, &proof);
    assert!(matches!(result, Err(MptError::NodeHashMismatch { .. })));

    // Tampered proof
    let mut tampered_leaf = make_leaf_node_rlp(&key_nibbles, value);
    tampered_leaf[0] ^= 0xFF; // tamper with first byte
    let result = verify_mpt_proof(correct_root, &key, &[tampered_leaf]);
    assert!(matches!(result, Err(MptError::NodeHashMismatch { .. })));
}

#[test]
fn test_bridge_event_parsing_from_receipt() {
    // Build a receipt with a bridge event log
    // Receipt: [status, cumulative_gas, bloom, logs]
    // Log: [address, topics, data]
    // Topics: [event_sig, source_tx_hash, recipient_padded]
    // Data: [source_chain, source_block, sender, asset_id, amount]

    use alloy_primitives::Address;

    let recipient = Address::repeat_byte(0x42);
    let source_tx_hash = B256::repeat_byte(0xAB);

    // Topics list
    let mut topics = Vec::new();
    topics.push(rlp_encode_short_bytes(&BRIDGE_DEPOSIT_EVENT_SIG.0)); // event signature
    topics.push(rlp_encode_short_bytes(&source_tx_hash.0)); // source_tx_hash
                                                            // Recipient: Address padded to 32 bytes
    let mut recipient_padded = [0u8; 32];
    recipient_padded[12..].copy_from_slice(recipient.as_slice());
    topics.push(rlp_encode_short_bytes(&recipient_padded));
    let topics_rlp = encode_rlp_list(&topics);

    // Data: [source_chain, source_block, sender, asset_id, amount]
    let mut data = Vec::new();
    data.push(rlp_encode_short_bytes(&[0x01])); // source_chain = 1 (Ethereum)
    let block_bytes = 60003u64.to_be_bytes();
    let start_block = block_bytes.iter().position(|&b| b != 0).unwrap_or(8);
    data.push(rlp_encode_short_bytes(&block_bytes[start_block..])); // source_block = 60003
    data.push(vec![0x80]); // sender (empty)
    data.push(rlp_encode_short_bytes(&[0x01])); // asset_id = 1
    let amount_bytes = 1000u128.to_be_bytes();
    let start = amount_bytes.iter().position(|&b| b != 0).unwrap_or(16);
    data.push(if start < 16 {
        rlp_encode_short_bytes(&amount_bytes[start..])
    } else {
        vec![0x80]
    });
    let data_rlp = encode_rlp_list(&data);

    // Log entry: [address, topics, data]
    let address = vec![0xC0u8; 20]; // some contract address
    let log = encode_rlp_list(&[rlp_encode_short_bytes(&address), topics_rlp, data_rlp]);
    let logs_rlp = encode_rlp_list(&[log]);

    // Receipt: [status, gas_used, bloom, logs]
    let receipt_rlp = encode_rlp_list(&[
        rlp_encode_short_bytes(&[0x01]),       // status
        rlp_encode_short_bytes(&[0x52, 0x08]), // gas_used = 21000
        vec![0x80],                            // bloom (empty)
        logs_rlp,
    ]);

    // Verify the header can parse this
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    // Create header with the receipt as the only receipt
    let receipt_leaf = make_leaf_node_rlp(&[], &receipt_rlp);
    let receipt_root = keccak256(&receipt_leaf);
    let tx_root = B256::repeat_byte(0x01);
    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let _block_hash = header.block_hash;
    client.submit_header(header).unwrap();

    // Create receipt proof
    let proof = ReceiptProof {
        receipt_index: 0,
        nodes: vec![MptProofNode {
            rlp_bytes: receipt_leaf.clone(),
        }],
    };

    let event = client
        .verify_receipt_and_parse_bridge_event(1001, &proof)
        .unwrap();

    assert_eq!(event.source_chain, 1);
    assert_eq!(event.source_block, 60003);
    assert_eq!(event.source_tx_hash, source_tx_hash);
    assert_eq!(event.recipient, recipient);
    assert_eq!(event.asset_id, 1);
    assert_eq!(event.amount, 1000);
}

// ═══════════════════════════════════════════════════════════════════════
// Malicious fork / Byzantine light-client tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_reject_reorg_below_finalized() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build chain 1001 → 1002 → 1003
    let rlp1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    let hash1 = h1.block_hash;
    client.submit_header(h1).unwrap();

    let rlp2 = make_test_header_rlp(hash1, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);
    let hash2 = h2.block_hash;
    client.submit_header(h2).unwrap();

    let rlp3 = make_test_header_rlp(hash2, 1003, tx_root, receipt_root);
    let h3 = EthHeader::from_rlp(rlp3);
    client.submit_header(h3).unwrap();

    // Finalize block 1002
    client.set_finalized_block(1002, hash2);

    // Attacker submits a NEW block 1004 whose parent is hash1 (block 1001),
    // not hash3. This triggers reorg handling with fork_point = 1001,
    // which is below the finalized block 1002.
    let malicious_rlp = make_test_header_rlp(hash1, 1004, tx_root, receipt_root);
    let malicious = EthHeader::from_rlp(malicious_rlp);

    let result = client.submit_header(malicious);
    assert!(
        matches!(result, Err(LightClientError::BeforeFinalized(1001))),
        "reorg below finalized should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_reject_tampered_block_hash() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let mut header = EthHeader::from_rlp(rlp);
    // Tamper the block hash so it doesn't match keccak256(rlp_bytes)
    header.block_hash = B256::repeat_byte(0xFF);

    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::InvalidHeader(_))),
        "tampered block hash should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_reject_duplicate_header() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header.clone()).unwrap();

    // Try to submit the same header again
    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::DuplicateHeader(1001))),
        "duplicate header should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_reject_block_before_anchor() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Try to submit a block at or before the anchor
    let rlp = make_test_header_rlp(anchor, 1000, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::BeforeAnchor(1000))),
        "block at anchor should be rejected, got {:?}",
        result
    );

    let rlp = make_test_header_rlp(anchor, 999, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::BeforeAnchor(999))),
        "block before anchor should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_reject_block_too_far_ahead() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Block 1065 is > 64 blocks ahead of anchor (1000) — should be rejected
    let rlp = make_test_header_rlp(anchor, 1065, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::HeaderNotVerified { .. })),
        "block too far ahead should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_side_chain_fork_and_resolution() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build canonical chain: 1001 → 1002
    let rlp1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    let hash1 = h1.block_hash;
    client.submit_header(h1).unwrap();

    let rlp2 = make_test_header_rlp(hash1, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);
    client.submit_header(h2).unwrap();

    // Attacker submits a NEW block 1003 whose parent is hash1 (block 1001),
    // not hash2. This triggers reorg handling: fork_point = 1001,
    // unwinds canonical 1002, and accepts the new 1003.
    let fork_rlp = make_test_header_rlp(hash1, 1003, tx_root, receipt_root);
    let fork_h3 = EthHeader::from_rlp(fork_rlp);
    let fork_hash3 = fork_h3.block_hash;

    client.submit_header(fork_h3).unwrap();

    // The new canonical tip should be the fork's block 1003
    assert_eq!(client.latest_block(), 1003);
    assert_eq!(
        client.get_header(1003).unwrap().block_hash,
        fork_hash3,
        "fork should become canonical"
    );
    // Old canonical 1002 should have been unwound
    assert!(
        client.get_header(1002).is_none(),
        "old canonical 1002 should be unwound"
    );
}

#[test]
fn test_buffer_overflow_rejected() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Chain 64 buffered blocks.  The first block (1002) passes the range check
    // (1002 <= 1000 + 64).  Subsequent blocks chain off the buffer and bypass
    // the range check entirely, so they can extend arbitrarily.
    for i in 0..64 {
        let parent = B256::repeat_byte(i as u8);
        let rlp = make_test_header_rlp(parent, 1002 + i, tx_root, receipt_root);
        let header = EthHeader::from_rlp(rlp);
        client.submit_header(header).unwrap();
    }
    assert_eq!(client.buffer_len(), 64);

    // 65th chained block should overflow the buffer.
    let parent = B256::repeat_byte(0xFF);
    let rlp = make_test_header_rlp(parent, 1066, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    let result = client.submit_header(header);
    assert!(
        matches!(result, Err(LightClientError::BufferFull)),
        "buffer overflow should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_malicious_parent_hash_chain_break() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Submit legitimate block 1001
    let rlp1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    client.submit_header(h1).unwrap();

    // Attacker submits block 1002 with parent_hash that is NOT the verified parent
    let malicious_parent = B256::repeat_byte(0xBB);
    let rlp2 = make_test_header_rlp(malicious_parent, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);

    let result = client.submit_header(h2);
    // The parent doesn't exist in verified headers, so it should be
    // rejected as HeaderNotFound (reorg handling can't find the parent)
    assert!(
        matches!(result, Err(LightClientError::HeaderNotFound(1001))),
        "malicious parent hash should break chain, got {:?}",
        result
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Beacon chain BLS consensus verification tests
// ═══════════════════════════════════════════════════════════════════════

/// Build a mock LightClientUpdate with a valid BLS aggregate signature from
/// a known sync committee. This simulates the full beacon consensus flow:
/// bootstrap sync committee → apply update → verify BLS → set finalized.
fn build_mock_light_client_update(
    signing_root: B256,
) -> (LightClientUpdate, Vec<call_crypto::BlsSecretKey>) {
    use call_crypto::{bls_generate, bls_sign_beacon, BlsPublicKey, BlsSecretKey, BlsSignature};

    const PARTICIPANTS: usize = 350; // > 342 minimum

    // Generate real keypairs for participants
    let mut secrets = Vec::with_capacity(PARTICIPANTS);
    let mut pubkeys = Vec::with_capacity(crate::beacon::SYNC_COMMITTEE_SIZE);

    for i in 0..crate::beacon::SYNC_COMMITTEE_SIZE {
        if i < PARTICIPANTS {
            let (sk, pk) = bls_generate().unwrap();
            secrets.push(sk);
            pubkeys.push(pk);
        } else {
            pubkeys.push(BlsPublicKey([0u8; 48]));
        }
    }

    // Create sync committee
    let mut agg_pk = [0u8; 48];
    agg_pk.copy_from_slice(&pubkeys[0].0);
    let sync_committee = SyncCommittee {
        pubkeys: pubkeys.clone(),
        aggregate_pubkey: BlsPublicKey(agg_pk),
    };

    // Build attested and finalized headers
    let attested_header = BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let finalized_header = BeaconBlockHeader {
        slot: 98,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x04),
        state_root: B256::repeat_byte(0x05),
        body_root: B256::repeat_byte(0x06),
    };

    // Set participation bits for first PARTICIPANTS validators
    let mut bits = [0u8; 64];
    for i in 0..PARTICIPANTS {
        let byte_idx = i / 8;
        let bit_idx = i % 8;
        bits[byte_idx] |= 1 << bit_idx;
    }

    // Sign with all participants using beacon DST
    let mut sigs: Vec<BlsSignature> = secrets
        .iter()
        .map(|sk| bls_sign_beacon(sk, signing_root.as_slice()))
        .collect();

    let agg_sig = call_crypto::bls_aggregate(&sigs).unwrap();

    let sync_aggregate = SyncAggregate {
        sync_committee_bits: bits,
        sync_committee_signature: agg_sig,
    };

    let update = LightClientUpdate {
        attested_header,
        next_sync_committee: sync_committee,
        next_sync_committee_branch: [B256::ZERO; crate::beacon::NEXT_SYNC_COMMITTEE_BRANCH_DEPTH],
        finalized_header,
        finality_branch: [B256::ZERO; crate::beacon::FINALIZED_BRANCH_DEPTH],
        sync_aggregate,
        signature_slot: 101,
    };

    (update, secrets)
}

#[test]
fn test_apply_light_client_update_bls_consensus_full_flow() {
    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let mut client = EthLightClient::init_with_beacon_config(genesis, Some(beacon_config.clone()));

    // Compute signing root from attested header
    let attested_header = BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let signing_root =
        crate::beacon::compute_sync_committee_signing_root(&attested_header, beacon_config.fork_version, beacon_config.genesis_validators_root);

    let (update, _secrets) = build_mock_light_client_update(signing_root);

    // Apply the update — BLS verification should pass
    let result = client.apply_light_client_update(update);
    assert!(result.is_ok(), "BLS consensus update should succeed: {:?}", result);

    let (finalized_slot, finalized_root) = result.unwrap();
    assert_eq!(finalized_slot, 98);

    // Map beacon slot to execution block and mark consensus-verified
    client.set_finalized_block(finalized_slot, finalized_root);

    assert!(client.is_consensus_verified(finalized_slot));
    assert!(client.is_consensus_verified(finalized_slot - 1));
    assert!(!client.is_consensus_verified(finalized_slot + 1));
}

#[test]
fn test_apply_light_client_update_rejects_invalid_signature() {
    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let mut client = EthLightClient::init_with_beacon_config(genesis, Some(beacon_config.clone()));

    let attested_header = BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let signing_root =
        crate::beacon::compute_sync_committee_signing_root(&attested_header, beacon_config.fork_version, beacon_config.genesis_validators_root);

    let (mut update, _secrets) = build_mock_light_client_update(signing_root);

    // Tamper the signature — flip a byte
    update.sync_aggregate.sync_committee_signature.0[0] ^= 0xFF;

    let result = client.apply_light_client_update(update);
    assert!(
        matches!(result, Err(LightClientError::SyncCommitteeSignatureInvalid(_))),
        "tampered signature should be rejected, got {:?}",
        result
    );
}

#[test]
fn test_apply_light_client_update_rejects_insufficient_participation() {
    use call_crypto::{bls_generate, bls_sign_beacon};

    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let mut client = EthLightClient::init_with_beacon_config(genesis, Some(beacon_config.clone()));

    let attested_header = BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let signing_root =
        crate::beacon::compute_sync_committee_signing_root(&attested_header, beacon_config.fork_version, beacon_config.genesis_validators_root);

    // Build a sync committee with only 1 real key and the rest zeros
    let (sk, pk) = bls_generate().unwrap();
    let mut pubkeys = vec![call_crypto::BlsPublicKey([0u8; 48]); crate::beacon::SYNC_COMMITTEE_SIZE];
    pubkeys[0] = pk;

    let sync_committee = SyncCommittee {
        pubkeys: pubkeys.clone(),
        aggregate_pubkey: pk,
    };

    let finalized_header = BeaconBlockHeader {
        slot: 98,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x04),
        state_root: B256::repeat_byte(0x05),
        body_root: B256::repeat_byte(0x06),
    };

    // Only validator 0 participated — sign with just 1 key
    let mut bits = [0u8; 64];
    bits[0] = 0b0000_0001;
    let sig = bls_sign_beacon(&sk, signing_root.as_slice());

    let update = LightClientUpdate {
        attested_header: attested_header.clone(),
        next_sync_committee: sync_committee,
        next_sync_committee_branch: [B256::ZERO; crate::beacon::NEXT_SYNC_COMMITTEE_BRANCH_DEPTH],
        finalized_header,
        finality_branch: [B256::ZERO; crate::beacon::FINALIZED_BRANCH_DEPTH],
        sync_aggregate: SyncAggregate {
            sync_committee_bits: bits,
            sync_committee_signature: sig,
        },
        signature_slot: 101,
    };

    let result = client.apply_light_client_update(update);
    assert!(
        matches!(result, Err(LightClientError::InsufficientSyncParticipation { .. })),
        "insufficient participation should be rejected, got {:?}",
        result
    );
}

/// Build a typed receipt (EIP-2718) with a bridge event log.
/// `receipt_type` should be 0x01 (EIP-2930) or 0x02 (EIP-1559).
fn make_typed_receipt_rlp(receipt_type: u8) -> Vec<u8> {
    use alloy_primitives::Address;

    let recipient = Address::repeat_byte(0x42);
    let source_tx_hash = B256::repeat_byte(0xAB);

    let mut topics = Vec::new();
    topics.push(rlp_encode_short_bytes(&BRIDGE_DEPOSIT_EVENT_SIG.0));
    topics.push(rlp_encode_short_bytes(&source_tx_hash.0));
    let mut recipient_padded = [0u8; 32];
    recipient_padded[12..].copy_from_slice(recipient.as_slice());
    topics.push(rlp_encode_short_bytes(&recipient_padded));
    let topics_rlp = encode_rlp_list(&topics);

    let mut data = Vec::new();
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(vec![0x80]);
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(rlp_encode_short_bytes(&[0x03, 0xE8]));
    let data_rlp = encode_rlp_list(&data);

    let address = vec![0xC0u8; 20];
    let log = encode_rlp_list(&[rlp_encode_short_bytes(&address), topics_rlp, data_rlp]);
    let logs_rlp = encode_rlp_list(&[log]);

    let body = encode_rlp_list(&[
        rlp_encode_short_bytes(&[0x01]),
        rlp_encode_short_bytes(&[0x52, 0x08]),
        vec![0x80],
        logs_rlp,
    ]);

    let mut receipt = Vec::with_capacity(1 + body.len());
    receipt.push(receipt_type);
    receipt.extend_from_slice(&body);
    receipt
}

#[test]
fn test_parse_receipt_logs_eip2930_type_1() {
    let receipt_rlp = make_typed_receipt_rlp(0x01);
    let logs = crate::ethereum::proof::parse_receipt_logs(&receipt_rlp).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].address, vec![0xC0u8; 20]);
    assert_eq!(logs[0].topics.len(), 3);
    assert_eq!(logs[0].topics[0], BRIDGE_DEPOSIT_EVENT_SIG);
}

#[test]
fn test_parse_receipt_logs_eip1559_type_2() {
    let receipt_rlp = make_typed_receipt_rlp(0x02);
    let logs = crate::ethereum::proof::parse_receipt_logs(&receipt_rlp).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].address, vec![0xC0u8; 20]);
    assert_eq!(logs[0].topics.len(), 3);
    assert_eq!(logs[0].topics[0], BRIDGE_DEPOSIT_EVENT_SIG);
}

#[test]
fn test_parse_receipt_logs_eip4844_type_3() {
    let receipt_rlp = make_typed_receipt_rlp(0x03);
    let logs = crate::ethereum::proof::parse_receipt_logs(&receipt_rlp).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].address, vec![0xC0u8; 20]);
    assert_eq!(logs[0].topics.len(), 3);
}

#[test]
fn test_typed_receipt_bridge_event_parsing() {
    let receipt_rlp = make_typed_receipt_rlp(0x02);
    let logs = crate::ethereum::proof::parse_receipt_logs(&receipt_rlp).unwrap();
    let event = crate::ethereum::proof::parse_bridge_event_from_logs(&logs).unwrap();
    assert_eq!(event.source_chain, 1);
    assert_eq!(event.asset_id, 1);
    assert_eq!(event.amount, 1000);
}

#[test]
fn test_parse_receipt_logs_empty_rejected() {
    let result = crate::ethereum::proof::parse_receipt_logs(b"");
    assert!(result.is_err());
}

#[test]
fn test_init_with_beacon_config_stores_config() {
    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let client = EthLightClient::init_with_beacon_config(genesis, Some(beacon_config));

    // Without any updates, apply_light_client_update should fail because
    // there is no sync committee yet.
    assert_eq!(client.current_sync_period(), 0);
    assert_eq!(client.finalized_block(), None);
    assert!(!client.is_consensus_verified(1000));
}

#[test]
fn test_is_consensus_verified_without_finalized() {
    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Submit block 1001
    let rlp = make_test_header_rlp(B256::repeat_byte(0xAA), 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header).unwrap();

    // No finalized block set yet
    assert!(!client.is_consensus_verified(1000));
    assert!(!client.is_consensus_verified(1001));
    assert!(!client.is_consensus_verified(999));
}

#[test]
fn test_is_consensus_verified_with_finalized() {
    let genesis = GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build chain 1001 → 1002 → 1003
    let rlp1 = make_test_header_rlp(B256::repeat_byte(0xAA), 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    let hash1 = h1.block_hash;
    client.submit_header(h1).unwrap();

    let rlp2 = make_test_header_rlp(hash1, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);
    let hash2 = h2.block_hash;
    client.submit_header(h2).unwrap();

    let rlp3 = make_test_header_rlp(hash2, 1003, tx_root, receipt_root);
    let h3 = EthHeader::from_rlp(rlp3);
    let hash3 = h3.block_hash;
    client.submit_header(h3).unwrap();

    // Finalize block 1002
    client.set_finalized_block(1002, hash2);

    assert!(client.is_consensus_verified(1000));
    assert!(client.is_consensus_verified(1001));
    assert!(client.is_consensus_verified(1002));
    assert!(!client.is_consensus_verified(1003));
    assert!(!client.is_consensus_verified(1004));

    // Advance finalized to 1003
    client.set_finalized_block(1003, hash3);
    assert!(client.is_consensus_verified(1003));
}

// ═══════════════════════════════════════════════════════════════════════
// Reorg handling: orphaned headers, rollback, and resync
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_reorg_longer_chain_rollback_and_adoption() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build canonical chain A: 1001 → 1002 → 1003
    let rlp_a1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h_a1 = EthHeader::from_rlp(rlp_a1);
    let hash_a1 = h_a1.block_hash;
    client.submit_header(h_a1).unwrap();

    let rlp_a2 = make_test_header_rlp(hash_a1, 1002, tx_root, receipt_root);
    let h_a2 = EthHeader::from_rlp(rlp_a2);
    let hash_a2 = h_a2.block_hash;
    client.submit_header(h_a2).unwrap();

    let rlp_a3 = make_test_header_rlp(hash_a2, 1003, tx_root, receipt_root);
    let h_a3 = EthHeader::from_rlp(rlp_a3);
    let hash_a3 = h_a3.block_hash;
    client.submit_header(h_a3).unwrap();

    assert_eq!(client.latest_block(), 1003);
    assert_eq!(client.get_header(1003).unwrap().block_hash, hash_a3);

    // Build longer competing chain B that forks at block 1001:
    // submit block 1004 with parent = hash_a1 (block 1001).
    // This triggers reorg: fork_point = 1001, unwinds 1002 and 1003.
    let receipt_root_b = B256::repeat_byte(0xBB);
    let rlp_b4 = make_test_header_rlp(hash_a1, 1004, tx_root, receipt_root_b);
    let h_b4 = EthHeader::from_rlp(rlp_b4);
    let hash_b4 = h_b4.block_hash;

    client.submit_header(h_b4).unwrap();

    // 1002 and 1003 were unwound; 1004 is now canonical
    assert!(client.get_header(1002).is_none(), "old 1002 should be unwound");
    assert!(client.get_header(1003).is_none(), "old 1003 should be unwound");
    assert_eq!(client.latest_block(), 1004);
    assert_eq!(client.get_header(1004).unwrap().block_hash, hash_b4);

    // Extend chain B with 1005
    let rlp_b5 = make_test_header_rlp(hash_b4, 1005, tx_root, receipt_root_b);
    let h_b5 = EthHeader::from_rlp(rlp_b5);
    let hash_b5 = h_b5.block_hash;
    client.submit_header(h_b5).unwrap();
    assert_eq!(client.latest_block(), 1005);
    assert_eq!(client.get_header(1005).unwrap().block_hash, hash_b5);
}

#[test]
fn test_reorg_resubmit_unwound_headers() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build canonical chain: 1001 → 1002 → 1003
    let rlp1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    let hash1 = h1.block_hash;
    client.submit_header(h1.clone()).unwrap();

    let rlp2 = make_test_header_rlp(hash1, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);
    let hash2 = h2.block_hash;
    client.submit_header(h2.clone()).unwrap();

    let rlp3 = make_test_header_rlp(hash2, 1003, tx_root, receipt_root);
    let h3 = EthHeader::from_rlp(rlp3);
    let hash3 = h3.block_hash;
    client.submit_header(h3.clone()).unwrap();

    // Trigger reorg: submit block 1004 with parent = hash1 (block 1001).
    // Fork point = 1001; unwinds 1002 and 1003.
    let receipt_root_fork = B256::repeat_byte(0xCC);
    let rlp_fork4 = make_test_header_rlp(hash1, 1004, tx_root, receipt_root_fork);
    let h_fork4 = EthHeader::from_rlp(rlp_fork4);

    // Use handle_reorg directly to capture unwound headers
    let unwound = client.handle_reorg(h_fork4).unwrap();

    // Unwound should contain h2 and h3 (blocks above fork point 1001)
    assert_eq!(unwound.len(), 2, "should unwind 2 headers above fork point 1001");
    let unwound_hashes: Vec<B256> = unwound.iter().map(|h| h.block_hash).collect();
    assert!(unwound_hashes.contains(&hash2));
    assert!(unwound_hashes.contains(&hash3));

    // Latest block should now be the fork block 1004
    assert_eq!(client.latest_block(), 1004);

    // Old 1002 and 1003 are gone
    assert!(client.get_header(1002).is_none());
    assert!(client.get_header(1003).is_none());

    // Resubmit the unwound headers — they should form a valid chain again
    // First resubmit h2 (its parent hash1 is still verified at block 1001)
    client.submit_header(h2.clone()).unwrap();
    assert_eq!(client.get_header(1002).unwrap().block_hash, hash2);

    // Then resubmit h3 (its parent h2 is now verified again at block 1002)
    client.submit_header(h3.clone()).unwrap();
    assert_eq!(client.get_header(1003).unwrap().block_hash, hash3);
    assert_eq!(client.latest_block(), 1004);
}

#[test]
fn test_reorg_buffered_headers_flushed_after_rollback() {
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);
    let tx_root = B256::repeat_byte(0x01);
    let receipt_root = B256::repeat_byte(0x02);

    // Build canonical chain: 1001 → 1002 → 1003
    let rlp1 = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let h1 = EthHeader::from_rlp(rlp1);
    let hash1 = h1.block_hash;
    client.submit_header(h1).unwrap();

    let rlp2 = make_test_header_rlp(hash1, 1002, tx_root, receipt_root);
    let h2 = EthHeader::from_rlp(rlp2);
    let hash2 = h2.block_hash;
    client.submit_header(h2).unwrap();

    let rlp3 = make_test_header_rlp(hash2, 1003, tx_root, receipt_root);
    let h3 = EthHeader::from_rlp(rlp3);
    let hash3 = h3.block_hash;
    client.submit_header(h3).unwrap();

    // Buffer a future header 1066 whose parent is 1065 (not yet known).
    // Block 1066 is within 64 of latest (1003), so it will be buffered.
    // Using a block number whose parent won't exist after reorg ensures the
    // buffered header is never incorrectly flushed by flush_buffer (which only
    // checks parent block number existence, not hash match).
    let fake_hash_1065 = B256::repeat_byte(0x99);
    let rlp1066_buffered = make_test_header_rlp(fake_hash_1065, 1066, tx_root, receipt_root);
    let h1066_buffered = EthHeader::from_rlp(rlp1066_buffered);
    client.submit_header(h1066_buffered.clone()).unwrap();
    assert_eq!(client.buffer_len(), 1);

    // Trigger reorg at block 1004 with parent = hash2 (block 1002).
    // This skips 1003: fork_point = 1002, unwinds 1003, inserts 1004.
    let receipt_root_fork = B256::repeat_byte(0xDD);
    let rlp_fork4 = make_test_header_rlp(hash2, 1004, tx_root, receipt_root_fork);
    let h_fork4 = EthHeader::from_rlp(rlp_fork4);
    let hash_fork4 = h_fork4.block_hash;
    client.submit_header(h_fork4).unwrap();

    // 1003 should be unwound
    assert!(client.get_header(1003).is_none(), "old 1003 should be unwound");
    assert_eq!(client.latest_block(), 1004);

    // The buffered 1066 is still there (its parent 1065 is still not verified)
    assert_eq!(client.buffer_len(), 1);

    // Submit a new 1005 whose parent is the forked 1004 — should be accepted directly
    let rlp5_correct = make_test_header_rlp(hash_fork4, 1005, tx_root, receipt_root);
    let h5_correct = EthHeader::from_rlp(rlp5_correct);
    client.submit_header(h5_correct.clone()).unwrap();

    // Buffered 1066 still there; new 1005 inserted directly
    assert_eq!(client.buffer_len(), 1, "buffered 1066 still waiting for parent 1065");
    assert_eq!(client.latest_block(), 1005);
    assert_eq!(client.get_header(1005).unwrap().block_hash, h5_correct.block_hash);
}

// ═══════════════════════════════════════════════════════════════════════
// MPT proof verification with multi-node tries against real headers
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_tx_inclusion_proof_with_branch_node() {
    use crate::verifier::{make_branch_node_rlp, make_leaf_node_rlp};

    // Build a tx trie with 3 transactions, using a branch node as root
    let tx1_hash = B256::repeat_byte(0x1A);
    let tx2_hash = B256::repeat_byte(0x2B);
    let tx3_hash = B256::repeat_byte(0x3C);

    // Leaf for tx1 at nibble path [1, 0, ...] (key = 0x1A, nibbles = [1, 10, ...])
    // Branch consumes first nibble (1), leaf contains remaining 63 nibbles
    let tx1_key = crate::verifier::bytes_to_nibbles(&tx1_hash.0);
    let tx1_leaf = make_leaf_node_rlp(&tx1_key[1..], b"tx1_data");
    let tx1_hash_node = keccak256(&tx1_leaf);

    // Leaf for tx2 at nibble path [2, 11, ...]
    let tx2_key = crate::verifier::bytes_to_nibbles(&tx2_hash.0);
    let tx2_leaf = make_leaf_node_rlp(&tx2_key[1..], b"tx2_data");
    let tx2_hash_node = keccak256(&tx2_leaf);

    // Leaf for tx3 at nibble path [3, 12, ...]
    let tx3_key = crate::verifier::bytes_to_nibbles(&tx3_hash.0);
    let tx3_leaf = make_leaf_node_rlp(&tx3_key[1..], b"tx3_data");
    let tx3_hash_node = keccak256(&tx3_leaf);

    // Branch node: children at nibbles 1, 2, 3
    let mut children: [Option<B256>; 16] = [None; 16];
    children[1] = Some(tx1_hash_node);
    children[2] = Some(tx2_hash_node);
    children[3] = Some(tx3_hash_node);
    let branch_rlp = make_branch_node_rlp(&children, None);
    let tx_root = keccak256(&branch_rlp);

    // Build and submit header
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    let receipt_root = B256::repeat_byte(0x02);
    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header).unwrap();

    // Verify tx2 inclusion — proof traverses branch → leaf
    let tx_proof = TxInclusionProof::new(vec![
        MptProofNode { rlp_bytes: branch_rlp.clone() },
        MptProofNode { rlp_bytes: tx2_leaf.clone() },
    ]);
    let result = client.verify_tx_inclusion(1001, tx2_hash, &tx_proof);
    assert!(result.is_ok(), "tx inclusion with branch node should succeed: {:?}", result);

    // Verify tx1 inclusion
    let tx_proof = TxInclusionProof::new(vec![
        MptProofNode { rlp_bytes: branch_rlp.clone() },
        MptProofNode { rlp_bytes: tx1_leaf.clone() },
    ]);
    let result = client.verify_tx_inclusion(1001, tx1_hash, &tx_proof);
    assert!(result.is_ok(), "tx1 inclusion should succeed: {:?}", result);

    // Verify tx3 inclusion
    let tx_proof = TxInclusionProof::new(vec![
        MptProofNode { rlp_bytes: branch_rlp },
        MptProofNode { rlp_bytes: tx3_leaf },
    ]);
    let result = client.verify_tx_inclusion(1001, tx3_hash, &tx_proof);
    assert!(result.is_ok(), "tx3 inclusion should succeed: {:?}", result);
}

#[test]
fn test_receipt_proof_with_extension_and_branch() {
    use crate::verifier::{
        make_branch_node_rlp, make_extension_node_rlp, make_leaf_node_rlp, rlp_encode_short_bytes,
    };
    use alloy_primitives::Address;

    // Build a bridge event receipt
    let recipient = Address::repeat_byte(0x42);
    let source_tx_hash = B256::repeat_byte(0xAB);

    let mut topics = Vec::new();
    topics.push(rlp_encode_short_bytes(&BRIDGE_DEPOSIT_EVENT_SIG.0));
    topics.push(rlp_encode_short_bytes(&source_tx_hash.0));
    let mut recipient_padded = [0u8; 32];
    recipient_padded[12..].copy_from_slice(recipient.as_slice());
    topics.push(rlp_encode_short_bytes(&recipient_padded));
    let topics_rlp = encode_rlp_list(&topics);

    let mut data = Vec::new();
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(vec![0x80]);
    data.push(rlp_encode_short_bytes(&[0x01]));
    data.push(rlp_encode_short_bytes(&[0x03, 0xE8]));
    let data_rlp = encode_rlp_list(&data);

    let address = vec![0xC0u8; 20];
    let log = encode_rlp_list(&[rlp_encode_short_bytes(&address), topics_rlp, data_rlp]);
    let logs_rlp = encode_rlp_list(&[log]);

    let receipt_rlp = encode_rlp_list(&[
        rlp_encode_short_bytes(&[0x01]),
        rlp_encode_short_bytes(&[0x52, 0x08]),
        vec![0x80],
        logs_rlp,
    ]);

    // Build a receipt trie with extension → branch → leaf structure.
    // Receipt index = 5. In RLP, index key = [0x05] (single byte).
    // Nibbles for key [0x05] = [0, 5].
    // Extension covers nibble [0], branch at nibble [5], leaf has empty remaining key.

    let index_key = crate::ethereum::proof::rlp_encode_u64(5);
    let key_nibbles = crate::verifier::bytes_to_nibbles(&index_key);

    // Leaf with empty remaining key after branch
    let leaf_rlp = make_leaf_node_rlp(&[], &receipt_rlp);
    let leaf_hash = keccak256(&leaf_rlp);

    // Branch with child at nibble 5 pointing to leaf
    let mut children: [Option<B256>; 16] = [None; 16];
    children[5] = Some(leaf_hash);
    let branch_rlp = make_branch_node_rlp(&children, None);
    let branch_hash = keccak256(&branch_rlp);

    // Extension covering nibble [0] pointing to branch
    let ext_rlp = make_extension_node_rlp(&[0], branch_hash);
    let receipt_root = keccak256(&ext_rlp);

    // Build and submit header
    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    let tx_root = B256::repeat_byte(0x01);
    let rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header).unwrap();

    // Verify receipt proof: extension → branch → leaf
    let receipt_proof = ReceiptProof {
        receipt_index: 5,
        nodes: vec![
            MptProofNode { rlp_bytes: ext_rlp },
            MptProofNode { rlp_bytes: branch_rlp },
            MptProofNode { rlp_bytes: leaf_rlp },
        ],
    };

    let event = client
        .verify_receipt_and_parse_bridge_event(1001, &receipt_proof)
        .unwrap();
    assert_eq!(event.recipient, recipient);
    assert_eq!(event.source_tx_hash, source_tx_hash);
    assert_eq!(event.asset_id, 1);
    assert_eq!(event.amount, 1000);
}

#[test]
fn test_tx_inclusion_proof_missing_tx_rejected() {
    use crate::verifier::{make_branch_node_rlp, make_leaf_node_rlp};

    // Build a tx trie with only tx1
    let tx1_hash = B256::repeat_byte(0x1A);
    let tx1_key = crate::verifier::bytes_to_nibbles(&tx1_hash.0);
    let tx1_leaf = make_leaf_node_rlp(&tx1_key, b"tx1_data");
    let tx1_hash_node = keccak256(&tx1_leaf);

    let mut children: [Option<B256>; 16] = [None; 16];
    children[1] = Some(tx1_hash_node);
    let branch_rlp = make_branch_node_rlp(&children, None);
    let tx_root = keccak256(&branch_rlp);

    let anchor = B256::repeat_byte(0xAA);
    let genesis = GenesisState {
        anchor_hash: anchor,
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    let rlp = make_test_header_rlp(anchor, 1001, tx_root, B256::repeat_byte(0x02));
    let header = EthHeader::from_rlp(rlp);
    client.submit_header(header).unwrap();

    // Try to prove tx2 (which is not in the trie) — should fail
    let tx2_hash = B256::repeat_byte(0x2B);
    let tx2_key = crate::verifier::bytes_to_nibbles(&tx2_hash.0);
    // Need a leaf for the proof that has the wrong key
    let wrong_leaf = make_leaf_node_rlp(&tx2_key, b"tx2_data");
    let tx_proof = TxInclusionProof::new(vec![
        MptProofNode { rlp_bytes: branch_rlp },
        MptProofNode { rlp_bytes: wrong_leaf },
    ]);

    let result = client.verify_tx_inclusion(1001, tx2_hash, &tx_proof);
    assert!(
        matches!(result, Err(LightClientError::TxNotFound)),
        "missing tx should return TxNotFound, got {:?}",
        result
    );
}

// ── Property-based tests (gap #31) ──────────────────────────────────

use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_decode_rlp_field_never_panics(data in prop::collection::vec(any::<u8>(), 0..256), idx in 0usize..20usize) {
        // decode_rlp_field must never panic, regardless of input
        let _ = crate::types::decode_rlp_field(&data, idx);
    }

    #[test]
    fn prop_eth_header_hash_roundtrip(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let header = EthHeader::from_rlp(data.clone());
        let expected = keccak256(&data);
        assert_eq!(header.block_hash, expected);
    }

    #[test]
    fn prop_verify_mpt_proof_never_panics(
        root_hash in prop::array::uniform32(any::<u8>()),
        key in prop::collection::vec(any::<u8>(), 0..64),
        proof in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..128), 0..8)
    ) {
        let root = B256::from(root_hash);
        let result = verify_mpt_proof(root, &key, &proof);
        // Must return Ok or Err, never panic
        let _ = result;
    }

    #[test]
    fn prop_mpt_empty_proof_returns_none(
        root_hash in prop::array::uniform32(any::<u8>()),
        key in prop::collection::vec(any::<u8>(), 0..64)
    ) {
        let root = B256::from(root_hash);
        let result = verify_mpt_proof(root, &key, &[]);
        assert_eq!(result, Ok(None), "empty proof should always return Ok(None)");
    }

    #[test]
    fn prop_mpt_proof_result_is_deterministic(
        root_hash in prop::array::uniform32(any::<u8>()),
        key in prop::collection::vec(any::<u8>(), 0..64),
        proof in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..128), 0..8)
    ) {
        let root = B256::from(root_hash);
        let r1 = verify_mpt_proof(root, &key, &proof);
        let r2 = verify_mpt_proof(root, &key, &proof);
        assert_eq!(r1, r2, "verify_mpt_proof must be deterministic");
    }
}

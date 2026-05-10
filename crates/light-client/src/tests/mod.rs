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

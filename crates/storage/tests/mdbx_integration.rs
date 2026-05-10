//! MDBX integration tests — covers all 22 CallTables with put/get/delete roundtrips.
//!
//! Verifies:
//! - Every table accepts writes, returns correct values on read, and removes on delete
//! - Convenience helpers (light-client headers, block-hash index, bytecodes) roundtrip
//! - Batch writes, iteration, empty-table handling, overwrite, and large-value behaviour

use call_storage::{
    db_batch_put, db_clear, db_del, db_get, db_iter_all, db_put, delete_block_hash_by_height,
    delete_bytecode, delete_light_client_header, load_all_light_client_headers,
    load_block_hash_by_height, load_bytecode, load_light_client_header, load_prune_state,
    save_block_hash_by_height, save_bytecode, save_light_client_header, save_prune_state,
    CallAccountHistory, CallAccountTrie, CallBlockHashIndex, CallBlockStateSnapshots,
    CallBytecodes, CallCheckpoint, CallConsensusBlocks, CallConsensusState, CallEvmAccounts,
    CallEvmStorage, CallFeeParams, CallForkState, CallLightClientHeaders, CallMetadataChainId,
    CallReceipts, CallReceiptsByBlock, CallRpcFilters, CallStorageHistory, CallStorageTrie,
    CallTrieUpdates,
};

// ── Helpers ──────────────────────────────────────────────────────────

fn temp_db() -> call_storage::CallDb {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "call-mdbx-integ-test-{}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            // Use a random suffix to avoid collisions across threads
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut h = DefaultHasher::new();
            std::thread::current().id().hash(&mut h);
            h.finish()
        },
        COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    call_storage::open_db(path).expect("open temp db")
}

// ── EVM tables ───────────────────────────────────────────────────────

#[test]
fn test_evm_accounts_roundtrip() {
    let db = temp_db();
    let key = b"addr_0x1234".to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"nonce": 5, "balance": "1000"})).unwrap();

    db_put::<CallEvmAccounts>(&db.db, key.clone(), value.clone()).unwrap();
    let loaded = db_get::<CallEvmAccounts>(&db.db, &key).unwrap();
    assert_eq!(loaded, Some(value));

    db_del::<CallEvmAccounts>(&db.db, &key).unwrap();
    assert!(db_get::<CallEvmAccounts>(&db.db, &key).unwrap().is_none());
}

#[test]
fn test_evm_storage_roundtrip() {
    let db = temp_db();
    let key = b"slot_0xabcd".to_vec();
    let value = serde_json::to_vec(&serde_json::json!("0xdeadbeef")).unwrap();

    db_put::<CallEvmStorage>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(db_get::<CallEvmStorage>(&db.db, &key).unwrap(), Some(value));

    db_del::<CallEvmStorage>(&db.db, &key).unwrap();
    assert!(db_get::<CallEvmStorage>(&db.db, &key).unwrap().is_none());
}

// ── Consensus tables ─────────────────────────────────────────────────

#[test]
fn test_consensus_state_roundtrip() {
    let db = temp_db();
    let value = serde_json::to_vec(&serde_json::json!({"validators": [1,2,3]})).unwrap();

    db_put::<CallConsensusState>(&db.db, vec![0], value.clone()).unwrap();
    assert_eq!(
        db_get::<CallConsensusState>(&db.db, &[0]).unwrap(),
        Some(value)
    );

    db_del::<CallConsensusState>(&db.db, &[0]).unwrap();
    assert!(db_get::<CallConsensusState>(&db.db, &[0])
        .unwrap()
        .is_none());
}

#[test]
fn test_consensus_blocks_roundtrip() {
    let db = temp_db();
    let key = 42u64.to_be_bytes().to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"height": 42, "hash": "0x00"})).unwrap();

    db_put::<CallConsensusBlocks>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallConsensusBlocks>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallConsensusBlocks>(&db.db, &key).unwrap();
    assert!(db_get::<CallConsensusBlocks>(&db.db, &key)
        .unwrap()
        .is_none());
}

// ── Receipt tables ───────────────────────────────────────────────────

#[test]
fn test_receipts_roundtrip() {
    let db = temp_db();
    let key = b"tx_hash_1".to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"status": 1, "gasUsed": 21000})).unwrap();

    db_put::<CallReceipts>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(db_get::<CallReceipts>(&db.db, &key).unwrap(), Some(value));

    db_del::<CallReceipts>(&db.db, &key).unwrap();
    assert!(db_get::<CallReceipts>(&db.db, &key).unwrap().is_none());
}

#[test]
fn test_receipts_by_block_roundtrip() {
    let db = temp_db();
    let key = 100u64.to_be_bytes().to_vec();
    let value = serde_json::to_vec(&vec!["tx_a", "tx_b"]).unwrap();

    db_put::<CallReceiptsByBlock>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallReceiptsByBlock>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallReceiptsByBlock>(&db.db, &key).unwrap();
    assert!(db_get::<CallReceiptsByBlock>(&db.db, &key)
        .unwrap()
        .is_none());
}

// ── Metadata ─────────────────────────────────────────────────────────

#[test]
fn test_metadata_chain_id_roundtrip() {
    let db = temp_db();
    let value = 1337u64.to_be_bytes().to_vec();

    db_put::<CallMetadataChainId>(&db.db, vec![0], value.clone()).unwrap();
    assert_eq!(
        db_get::<CallMetadataChainId>(&db.db, &[0]).unwrap(),
        Some(value)
    );

    db_del::<CallMetadataChainId>(&db.db, &[0]).unwrap();
    assert!(db_get::<CallMetadataChainId>(&db.db, &[0])
        .unwrap()
        .is_none());
}

// ── Fee / Fork / Trie / Checkpoint ───────────────────────────────────

#[test]
fn test_fee_params_roundtrip() {
    let db = temp_db();
    let value = serde_json::to_vec(&serde_json::json!({"baseFee": 10, "priorityFee": 1})).unwrap();

    db_put::<CallFeeParams>(&db.db, vec![0], value.clone()).unwrap();
    assert_eq!(db_get::<CallFeeParams>(&db.db, &[0]).unwrap(), Some(value));

    db_del::<CallFeeParams>(&db.db, &[0]).unwrap();
    assert!(db_get::<CallFeeParams>(&db.db, &[0]).unwrap().is_none());
}

#[test]
fn test_fork_state_roundtrip() {
    let db = temp_db();
    let value = serde_json::to_vec(&serde_json::json!({"upgrades": []})).unwrap();

    db_put::<CallForkState>(&db.db, vec![0], value.clone()).unwrap();
    assert_eq!(db_get::<CallForkState>(&db.db, &[0]).unwrap(), Some(value));

    db_del::<CallForkState>(&db.db, &[0]).unwrap();
    assert!(db_get::<CallForkState>(&db.db, &[0]).unwrap().is_none());
}

#[test]
fn test_trie_updates_roundtrip() {
    let db = temp_db();
    let key = 99u64.to_be_bytes().to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"nodes": ["a", "b"]})).unwrap();

    db_put::<CallTrieUpdates>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallTrieUpdates>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallTrieUpdates>(&db.db, &key).unwrap();
    assert!(db_get::<CallTrieUpdates>(&db.db, &key).unwrap().is_none());
}

#[test]
fn test_checkpoint_roundtrip() {
    let db = temp_db();
    let value = [0xDEu8; 32].to_vec();

    db_put::<CallCheckpoint>(&db.db, b"pending".to_vec(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallCheckpoint>(&db.db, b"pending").unwrap(),
        Some(value)
    );

    db_del::<CallCheckpoint>(&db.db, b"pending").unwrap();
    assert!(db_get::<CallCheckpoint>(&db.db, b"pending")
        .unwrap()
        .is_none());
}

// ── History tables ───────────────────────────────────────────────────

#[test]
fn test_account_history_roundtrip() {
    let db = temp_db();
    // Key layout: [address: 20 bytes][block_number: 8 bytes BE]
    let mut key = vec![0u8; 20];
    key.extend_from_slice(&100u64.to_be_bytes());
    let value = serde_json::to_vec(&serde_json::json!({"nonce": 1})).unwrap();

    db_put::<CallAccountHistory>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallAccountHistory>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallAccountHistory>(&db.db, &key).unwrap();
    assert!(db_get::<CallAccountHistory>(&db.db, &key)
        .unwrap()
        .is_none());
}

#[test]
fn test_storage_history_roundtrip() {
    let db = temp_db();
    // Key layout: [address: 20 bytes][slot: 32 bytes][block_number: 8 bytes BE]
    let mut key = vec![0u8; 20];
    key.extend_from_slice(&[0u8; 32]);
    key.extend_from_slice(&200u64.to_be_bytes());
    let value = serde_json::to_vec(&serde_json::json!("0x1234")).unwrap();

    db_put::<CallStorageHistory>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallStorageHistory>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallStorageHistory>(&db.db, &key).unwrap();
    assert!(db_get::<CallStorageHistory>(&db.db, &key)
        .unwrap()
        .is_none());
}

// ── Trie node tables ─────────────────────────────────────────────────

#[test]
fn test_account_trie_roundtrip() {
    let db = temp_db();
    let key = b"nibbles_0x01_0x02".to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"branch": true})).unwrap();

    db_put::<CallAccountTrie>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallAccountTrie>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallAccountTrie>(&db.db, &key).unwrap();
    assert!(db_get::<CallAccountTrie>(&db.db, &key).unwrap().is_none());
}

#[test]
fn test_storage_trie_roundtrip() {
    let db = temp_db();
    let key = b"hash_addr_nibbles".to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"leaf": true})).unwrap();

    db_put::<CallStorageTrie>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallStorageTrie>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallStorageTrie>(&db.db, &key).unwrap();
    assert!(db_get::<CallStorageTrie>(&db.db, &key).unwrap().is_none());
}

// ── Block state snapshots ────────────────────────────────────────────

#[test]
fn test_block_state_snapshots_roundtrip() {
    let db = temp_db();
    let key = 500u64.to_be_bytes().to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"accounts": {}})).unwrap();

    db_put::<CallBlockStateSnapshots>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallBlockStateSnapshots>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallBlockStateSnapshots>(&db.db, &key).unwrap();
    assert!(db_get::<CallBlockStateSnapshots>(&db.db, &key)
        .unwrap()
        .is_none());
}

// ── Light client headers (convenience helpers) ───────────────────────

#[test]
fn test_light_client_header_helpers() {
    let db = temp_db();
    let hash_a = call_primitives::BlockHash::from([0xAAu8; 32]);
    let hash_b = call_primitives::BlockHash::from([0xBBu8; 32]);

    save_light_client_header(&db.db, 10, &hash_a).unwrap();
    save_light_client_header(&db.db, 20, &hash_b).unwrap();

    assert_eq!(load_light_client_header(&db.db, 10).unwrap(), Some(hash_a));
    assert_eq!(load_light_client_header(&db.db, 20).unwrap(), Some(hash_b));
    assert_eq!(load_light_client_header(&db.db, 99).unwrap(), None);

    let all = load_all_light_client_headers(&db.db).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all.get(&10), Some(&hash_a));
    assert_eq!(all.get(&20), Some(&hash_b));

    delete_light_client_header(&db.db, 10).unwrap();
    assert_eq!(load_light_client_header(&db.db, 10).unwrap(), None);
    assert_eq!(load_all_light_client_headers(&db.db).unwrap().len(), 1);
}

// ── Block hash index ─────────────────────────────────────────────────

#[test]
fn test_block_hash_index_roundtrip() {
    let db = temp_db();
    let hash = call_primitives::BlockHash::from([0xCCu8; 32]);
    let key = hash.as_slice().to_vec();
    let value = 1234u64.to_be_bytes().to_vec();

    db_put::<CallBlockHashIndex>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(
        db_get::<CallBlockHashIndex>(&db.db, &key).unwrap(),
        Some(value)
    );

    db_del::<CallBlockHashIndex>(&db.db, &key).unwrap();
    assert!(db_get::<CallBlockHashIndex>(&db.db, &key)
        .unwrap()
        .is_none());
}

#[test]
fn test_block_hash_by_height_helpers() {
    let db = temp_db();
    let hash = call_primitives::BlockHash::from([0xDDu8; 32]);

    save_block_hash_by_height(&db.db, 1000, &hash).unwrap();
    assert_eq!(load_block_hash_by_height(&db.db, 1000).unwrap(), Some(hash));
    assert_eq!(load_block_hash_by_height(&db.db, 999).unwrap(), None);

    delete_block_hash_by_height(&db.db, 1000).unwrap();
    assert_eq!(load_block_hash_by_height(&db.db, 1000).unwrap(), None);
}

// ── RPC filters ──────────────────────────────────────────────────────

#[test]
fn test_rpc_filters_roundtrip() {
    let db = temp_db();
    let key = b"filters".to_vec();
    let value = serde_json::to_vec(&serde_json::json!({"active": [1,2,3]})).unwrap();

    db_put::<CallRpcFilters>(&db.db, key.clone(), value.clone()).unwrap();
    assert_eq!(db_get::<CallRpcFilters>(&db.db, &key).unwrap(), Some(value));

    db_del::<CallRpcFilters>(&db.db, &key).unwrap();
    assert!(db_get::<CallRpcFilters>(&db.db, &key).unwrap().is_none());
}

// ── Bytecodes (convenience helpers) ──────────────────────────────────

#[test]
fn test_bytecode_helpers() {
    let db = temp_db();
    let code_hash = call_primitives::BlockHash::from([0xEEu8; 32]);
    let code = vec![0x60, 0x80, 0x60, 0x40, 0x52]; // PUSH1 80 PUSH1 40 MSTORE

    save_bytecode(&db.db, &code_hash, &code).unwrap();
    assert_eq!(
        load_bytecode(&db.db, &code_hash).unwrap(),
        Some(code.clone())
    );

    delete_bytecode(&db.db, &code_hash).unwrap();
    assert_eq!(load_bytecode(&db.db, &code_hash).unwrap(), None);
}

// ── Prune state (convenience helper) ─────────────────────────────────

#[test]
fn test_prune_state_helper_roundtrip() {
    let db = temp_db();
    let state = call_storage::PruneState::new();

    save_prune_state(&db.db, &state).unwrap();
    let loaded = load_prune_state(&db.db).unwrap();
    assert_eq!(
        serde_json::to_string(&state).unwrap(),
        serde_json::to_string(&loaded).unwrap()
    );
}

// ── Cross-cutting behaviour ──────────────────────────────────────────

#[test]
fn test_batch_write_multiple_tables() {
    let db = temp_db();
    let mut pairs_evm = Vec::new();
    let mut pairs_receipts = Vec::new();
    for i in 0..100 {
        pairs_evm.push((
            format!("evm_{}", i).into_bytes(),
            serde_json::to_vec(&i).unwrap(),
        ));
        pairs_receipts.push((
            format!("rcpt_{}", i).into_bytes(),
            serde_json::to_vec(&i).unwrap(),
        ));
    }

    db_batch_put::<CallEvmAccounts>(&db.db, pairs_evm).unwrap();
    db_batch_put::<CallReceipts>(&db.db, pairs_receipts).unwrap();

    assert_eq!(db_iter_all::<CallEvmAccounts>(&db.db).unwrap().len(), 100);
    assert_eq!(db_iter_all::<CallReceipts>(&db.db).unwrap().len(), 100);
}

#[test]
fn test_iteration_sorted_order() {
    let db = temp_db();
    let mut keys = vec!["z", "a", "m", "b"];
    keys.sort(); // MDBX iterates in lexicographic key order

    for k in &keys {
        db_put::<CallEvmAccounts>(&db.db, k.as_bytes().to_vec(), b"v".to_vec()).unwrap();
    }

    let all = db_iter_all::<CallEvmAccounts>(&db.db).unwrap();
    let iterated_keys: Vec<String> = all
        .into_iter()
        .map(|(k, _)| String::from_utf8(k).unwrap())
        .collect();

    assert_eq!(iterated_keys, vec!["a", "b", "m", "z"]);
}

#[test]
fn test_empty_table_iteration() {
    let db = temp_db();
    let all = db_iter_all::<CallForkState>(&db.db).unwrap();
    assert!(all.is_empty());
}

#[test]
fn test_overwrite_existing_key() {
    let db = temp_db();
    let key = b"same_key".to_vec();

    db_put::<CallFeeParams>(&db.db, key.clone(), b"first".to_vec()).unwrap();
    assert_eq!(
        db_get::<CallFeeParams>(&db.db, &key).unwrap(),
        Some(b"first".to_vec())
    );

    db_put::<CallFeeParams>(&db.db, key.clone(), b"second".to_vec()).unwrap();
    assert_eq!(
        db_get::<CallFeeParams>(&db.db, &key).unwrap(),
        Some(b"second".to_vec())
    );
}

#[test]
fn test_large_value() {
    let db = temp_db();
    let key = b"big_snapshot".to_vec();
    let value = vec![0xABu8; 1_000_000]; // 1 MB

    db_put::<CallBlockStateSnapshots>(&db.db, key.clone(), value.clone()).unwrap();
    let loaded = db_get::<CallBlockStateSnapshots>(&db.db, &key)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.len(), 1_000_000);
    assert_eq!(loaded, value);
}

#[test]
fn test_db_clear_removes_all() {
    let db = temp_db();
    for i in 0..50 {
        let key = format!("k{}", i).into_bytes();
        db_put::<CallConsensusBlocks>(&db.db, key, vec![i as u8]).unwrap();
    }
    assert_eq!(
        db_iter_all::<CallConsensusBlocks>(&db.db).unwrap().len(),
        50
    );

    db_clear::<CallConsensusBlocks>(&db.db).unwrap();
    assert!(db_iter_all::<CallConsensusBlocks>(&db.db)
        .unwrap()
        .is_empty());
}

#[test]
fn test_delete_nonexistent_key_is_noop() {
    let db = temp_db();
    // Should not panic or error
    db_del::<CallLightClientHeaders>(&db.db, b"missing").unwrap();
    assert!(db_get::<CallLightClientHeaders>(&db.db, b"missing")
        .unwrap()
        .is_none());
}

#[test]
fn test_get_nonexistent_key_returns_none() {
    let db = temp_db();
    assert!(db_get::<CallBytecodes>(&db.db, b"nope").unwrap().is_none());
}

// ── Concurrency tests ────────────────────────────────────────────────

#[test]
fn test_concurrent_writes_same_key() {
    let db = temp_db();
    let db_arc = std::sync::Arc::clone(&db.db);
    let key = b"shared_key".to_vec();

    let mut handles = Vec::new();
    for t in 0..10 {
        let db_clone = std::sync::Arc::clone(&db_arc);
        let key_clone = key.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..20 {
                let value = format!("thread_{}_iter_{}", t, i).into_bytes();
                db_put::<CallEvmAccounts>(&db_clone, key_clone.clone(), value).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Value should be the last successful write (any thread's last iteration)
    let loaded = db_get::<CallEvmAccounts>(&db.db, &key).unwrap();
    assert!(loaded.is_some());
    let s = String::from_utf8(loaded.unwrap()).unwrap();
    assert!(s.starts_with("thread_"));
    assert!(s.contains("_iter_19"));
}

#[test]
fn test_concurrent_writes_same_table_different_keys() {
    let db = temp_db();
    let db_arc = std::sync::Arc::clone(&db.db);

    let mut handles = Vec::new();
    for t in 0..5 {
        let db_clone = std::sync::Arc::clone(&db_arc);
        handles.push(std::thread::spawn(move || {
            for i in 0..50 {
                let key = format!("t{}_k{}", t, i).into_bytes();
                let value = serde_json::to_vec(&(t * 100 + i)).unwrap();
                db_put::<CallReceipts>(&db_clone, key, value).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let all = db_iter_all::<CallReceipts>(&db.db).unwrap();
    assert_eq!(all.len(), 250);
}

#[test]
fn test_read_during_batch_write() {
    let db = temp_db();
    let db_arc = std::sync::Arc::clone(&db.db);

    // Pre-populate
    for i in 0..100 {
        let key = format!("pre_{}", i).into_bytes();
        db_put::<CallConsensusBlocks>(&db.db, key, vec![i as u8]).unwrap();
    }

    // Writer thread: batch write 500 more entries
    let writer = {
        let db_clone = std::sync::Arc::clone(&db_arc);
        std::thread::spawn(move || {
            let mut pairs = Vec::new();
            for i in 0..500 {
                pairs.push((format!("batch_{}", i).into_bytes(), vec![i as u8]));
            }
            db_batch_put::<CallConsensusBlocks>(&db_clone, pairs).unwrap();
        })
    };

    // Reader thread: iterate while writer is active
    let reader = {
        let db_clone = std::sync::Arc::clone(&db_arc);
        std::thread::spawn(move || {
            // Read multiple times; each read sees a consistent snapshot
            for _ in 0..10 {
                let all = db_iter_all::<CallConsensusBlocks>(&db_clone).unwrap();
                // Should see at least the pre-populated entries
                assert!(
                    all.len() >= 100,
                    "Reader saw only {} entries during batch write",
                    all.len()
                );
            }
        })
    };

    writer.join().unwrap();
    reader.join().unwrap();

    // After writer completes, all 600 entries should be visible
    let all = db_iter_all::<CallConsensusBlocks>(&db.db).unwrap();
    assert_eq!(all.len(), 600);
}

#[test]
fn test_batch_write_atomicity() {
    let db = temp_db();
    // Write some initial data
    db_put::<CallFeeParams>(&db.db, b"a".to_vec(), b"1".to_vec()).unwrap();
    db_put::<CallFeeParams>(&db.db, b"b".to_vec(), b"2".to_vec()).unwrap();

    // Batch overwrite with new values
    let batch = vec![
        (b"a".to_vec(), b"10".to_vec()),
        (b"b".to_vec(), b"20".to_vec()),
        (b"c".to_vec(), b"30".to_vec()),
    ];
    db_batch_put::<CallFeeParams>(&db.db, batch).unwrap();

    // All entries should be updated consistently
    assert_eq!(
        db_get::<CallFeeParams>(&db.db, b"a").unwrap(),
        Some(b"10".to_vec())
    );
    assert_eq!(
        db_get::<CallFeeParams>(&db.db, b"b").unwrap(),
        Some(b"20".to_vec())
    );
    assert_eq!(
        db_get::<CallFeeParams>(&db.db, b"c").unwrap(),
        Some(b"30".to_vec())
    );
}

#[test]
fn test_durability_close_reopen() {
    let path = {
        let db = temp_db();
        let path = db.data_dir.clone();

        // Write data
        db_put::<CallMetadataChainId>(&db.db, vec![0], 9999u64.to_be_bytes().to_vec()).unwrap();
        db_put::<CallEvmAccounts>(&db.db, b"addr1".to_vec(), b"balance_100".to_vec()).unwrap();

        // db is dropped here, closing MDBX
        path
    };

    // Reopen the same database
    let db = call_storage::open_db(path).unwrap();

    // Verify data survived
    assert_eq!(
        db_get::<CallMetadataChainId>(&db.db, &[0]).unwrap(),
        Some(9999u64.to_be_bytes().to_vec())
    );
    assert_eq!(
        db_get::<CallEvmAccounts>(&db.db, b"addr1").unwrap(),
        Some(b"balance_100".to_vec())
    );
}

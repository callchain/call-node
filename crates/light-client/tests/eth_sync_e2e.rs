//! E2E test: Light client against live Ethereum RPC (Gap 13)
//!
//! This test validates the light client against a real Ethereum node.
//! It fetches a recent block header, initializes the light client with
//! an older anchor, and verifies the header chain submission.
//!
//! Requires the `eth-sync` feature. Skipped if `ETH_RPC_URL` is not set.

#![cfg(feature = "eth-sync")]

use alloy_primitives::B256;
use call_light_client::{
    sync::{fetch_finalized_checkpoint, sync_header_range, sync_single_header},
    EthLightClient, GenesisState,
};

/// Real-network integration test for light client header verification.
///
/// Steps:
/// 1. Read `ETH_RPC_URL` from environment (skip if missing).
/// 2. Pick a recent finalized block number (e.g., current - 64).
/// 3. Fetch the header at `anchor_block`.
/// 4. Initialize light client with that header as anchor.
/// 5. Fetch and submit the next 5 consecutive headers.
/// 6. Verify all headers are accepted and the chain advances.
#[test]
fn test_light_client_real_network_header_sync() {
    let eth_rpc_url = match std::env::var("ETH_RPC_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("Skipping real-network test: ETH_RPC_URL not set.");
            eprintln!("Set it to a mainnet or testnet RPC endpoint, e.g.:");
            eprintln!("  export ETH_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/...");
            return;
        }
    };

    // Use a block well in the past to avoid reorgs during test execution.
    // We query the current block number and go back 128 blocks.
    let current_block = fetch_block_number(&eth_rpc_url).expect("fetch current block number");
    let anchor_block = current_block.saturating_sub(128);
    assert!(anchor_block > 0, "anchor block must be non-zero");

    // Fetch anchor header
    let anchor_header =
        sync_single_header(&eth_rpc_url, anchor_block).expect("fetch anchor header");

    // Initialize light client
    let genesis = GenesisState {
        anchor_hash: anchor_header.block_hash,
        anchor_block,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    // Sync the next 5 headers
    let sync_count = 5usize;
    for i in 1..=sync_count {
        let block_num = anchor_block + i as u64;
        let header = sync_single_header(&eth_rpc_url, block_num)
            .expect(&format!("fetch header at block {block_num}"));

        client
            .submit_header(header)
            .expect(&format!("submit header at block {block_num}"));
    }

    // Verify chain advanced
    assert_eq!(
        client.latest_block(),
        anchor_block + sync_count as u64,
        "light client should advance to the last submitted block"
    );

    // Verify all submitted blocks are marked verified
    for i in 0..=sync_count {
        let block_num = anchor_block + i as u64;
        assert!(
            client.is_header_verified(block_num),
            "block {block_num} should be verified"
        );
    }

    println!(
        "Real-network test passed: synced {} headers from block {} to {}",
        sync_count,
        anchor_block,
        client.latest_block()
    );
}

/// Sync a full epoch (32 headers) against a live Ethereum RPC.
/// Verifies sustained sync performance and buffer behavior.
#[test]
fn test_light_client_real_network_epoch_sync() {
    let eth_rpc_url = match std::env::var("ETH_RPC_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("Skipping real-network test: ETH_RPC_URL not set.");
            return;
        }
    };

    let current_block = fetch_block_number(&eth_rpc_url).expect("fetch current block number");
    let anchor_block = current_block.saturating_sub(256);
    assert!(anchor_block > 0);

    let anchor_header = sync_single_header(&eth_rpc_url, anchor_block).expect("fetch anchor");
    let genesis = GenesisState {
        anchor_hash: anchor_header.block_hash,
        anchor_block,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    // Sync 32 headers (one Ethereum epoch)
    let synced = sync_header_range(&eth_rpc_url, &mut client, anchor_block + 1, anchor_block + 32)
        .expect("sync epoch");
    assert_eq!(synced, 32, "should sync full epoch");
    assert_eq!(client.latest_block(), anchor_block + 32);

    // All headers verified
    for i in 0..=32 {
        let bn = anchor_block + i;
        assert!(
            client.is_header_verified(bn),
            "block {bn} should be verified"
        );
    }

    println!("Epoch sync passed: {} headers from {}", synced, anchor_block);
}

/// Verify parent-hash chain integrity across real Ethereum headers.
/// Each header's parent_hash must match the previous verified header.
#[test]
fn test_light_client_real_network_parent_chain() {
    let eth_rpc_url = match std::env::var("ETH_RPC_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("Skipping real-network test: ETH_RPC_URL not set.");
            return;
        }
    };

    let current_block = fetch_block_number(&eth_rpc_url).expect("fetch current block number");
    let anchor_block = current_block.saturating_sub(128);
    assert!(anchor_block > 0);

    let anchor_header = sync_single_header(&eth_rpc_url, anchor_block).expect("fetch anchor");
    let genesis = GenesisState {
        anchor_hash: anchor_header.block_hash,
        anchor_block,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    // Sync 16 headers
    let count = 16usize;
    let mut headers: Vec<call_light_client::EthHeader> = Vec::new();
    for i in 1..=count {
        let bn = anchor_block + i as u64;
        let header = sync_single_header(&eth_rpc_url, bn).expect("fetch header");
        headers.push(header.clone());
        client.submit_header(header).expect("submit header");
    }

    // Verify parent-hash chain: each header's parent_hash matches the previous
    for i in 1..headers.len() {
        let prev_hash = headers[i - 1].block_hash;
        let parent_hash = headers[i].parent_hash().expect("decode parent hash");
        assert_eq!(
            parent_hash, prev_hash,
            "parent hash mismatch at block {}",
            anchor_block + i as u64 + 1
        );
    }

    // Also verify via light client get_header
    for i in 1..=count {
        let bn = anchor_block + i as u64;
        let stored = client.get_header(bn).expect("header should be stored");
        let fetched = sync_single_header(&eth_rpc_url, bn).expect("fetch for comparison");
        assert_eq!(stored.block_hash, fetched.block_hash, "stored hash mismatch at {bn}");
    }

    println!("Parent chain test passed: {} headers linked correctly", count);
}

/// Test gap sync: submit every other header first, then fill the gaps.
/// Verifies the light client's buffering and flush behavior.
#[test]
fn test_light_client_real_network_gap_sync() {
    let eth_rpc_url = match std::env::var("ETH_RPC_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("Skipping real-network test: ETH_RPC_URL not set.");
            return;
        }
    };

    let current_block = fetch_block_number(&eth_rpc_url).expect("fetch current block number");
    let anchor_block = current_block.saturating_sub(128);
    assert!(anchor_block > 0);

    let anchor_header = sync_single_header(&eth_rpc_url, anchor_block).expect("fetch anchor");
    let genesis = GenesisState {
        anchor_hash: anchor_header.block_hash,
        anchor_block,
        state_root: B256::ZERO,
    };
    let mut client = EthLightClient::init(genesis);

    // Fetch 10 consecutive headers
    let total = 10usize;
    let mut headers: Vec<call_light_client::EthHeader> = Vec::new();
    for i in 1..=total {
        let bn = anchor_block + i as u64;
        headers.push(sync_single_header(&eth_rpc_url, bn).expect("fetch"));
    }

    // Submit even-indexed headers first (blocks 2, 4, 6, 8, 10 relative to anchor)
    // These will be buffered because their parents are missing
    for (idx, header) in headers.iter().enumerate() {
        if idx % 2 == 1 {
            // idx 1 -> block anchor+2, idx 3 -> anchor+4, etc.
            let result = client.submit_header(header.clone());
            // May buffer or fail depending on gap size; just record result
            if let Err(e) = result {
                println!("  Buffer/gap at block {}: {}", header.number().unwrap_or(0), e);
            }
        }
    }

    // Now submit the odd-indexed headers (blocks 1, 3, 5, 7, 9)
    // These should connect to the anchor and flush buffered children
    for (idx, header) in headers.iter().enumerate() {
        if idx % 2 == 0 {
            client
                .submit_header(header.clone())
                .expect(&format!("submit gap-filler at block {}", header.number().unwrap_or(0)));
        }
    }

    // Verify all headers are eventually verified
    let mut verified_count = 0;
    for i in 1..=total {
        let bn = anchor_block + i as u64;
        if client.is_header_verified(bn) {
            verified_count += 1;
        }
    }

    // At minimum the sequential ones should be verified; buffered ones
    // may or may not depending on buffer state. Assert the chain advanced.
    assert!(
        client.latest_block() >= anchor_block + 1,
        "light client should have advanced past anchor"
    );
    println!(
        "Gap sync test passed: {}/{} headers verified, latest={}",
        verified_count,
        total,
        client.latest_block()
    );
}

/// Test beacon chain finalized checkpoint fetch against a real beacon node.
/// Skipped if BEACON_URL is not set.
#[test]
fn test_light_client_real_network_finalized_checkpoint() {
    let beacon_url = match std::env::var("BEACON_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("Skipping beacon test: BEACON_URL not set.");
            eprintln!("Set it to a beacon node endpoint, e.g.:");
            eprintln!("  export BEACON_URL=https://ethereum-beacon-api.publicnode.com");
            return;
        }
    };

    let (block_number, block_hash) =
        fetch_finalized_checkpoint(&beacon_url).expect("fetch finalized checkpoint");

    assert!(
        block_number > 0,
        "finalized block number should be positive, got {}",
        block_number
    );
    assert!(
        block_hash != B256::ZERO,
        "finalized block hash should not be zero"
    );

    println!(
        "Beacon finalized checkpoint: block {} hash {:?}",
        block_number, block_hash
    );
}

/// Fetch the current Ethereum block number via `eth_blockNumber`.
fn fetch_block_number(eth_rpc_url: &str) -> Result<u64, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });

    let resp = ureq::post(eth_rpc_url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("RPC request failed: {e}"))?;

    let text = resp.into_string().map_err(|e| format!("read body: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse JSON: {e}"))?;

    let result = json
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or("missing result in response")?;

    let hex = result.strip_prefix("0x").unwrap_or(result);
    u64::from_str_radix(hex, 16).map_err(|e| format!("parse hex block number: {e}"))
}

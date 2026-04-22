//! E2E test: Light client against live Ethereum RPC (Gap 13)
//!
//! This test validates the light client against a real Ethereum node.
//! It fetches a recent block header, initializes the light client with
//! an older anchor, and verifies the header chain submission.
//!
//! Requires the `eth-sync` feature. Skipped if `ETH_RPC_URL` is not set.

#![cfg(feature = "eth-sync")]

use call_light_client::{
    EthLightClient, GenesisState,
    sync::sync_single_header,
};
use alloy_primitives::B256;

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
    let anchor_header = sync_single_header(&eth_rpc_url, anchor_block
    ).expect("fetch anchor header");

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
        let header = sync_single_header(&eth_rpc_url, block_num
        ).expect(&format!("fetch header at block {block_num}"));

        client.submit_header(header)
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
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse JSON: {e}"))?;

    let result = json.get("result")
        .and_then(|v| v.as_str())
        .ok_or("missing result in response")?;

    let hex = result.strip_prefix("0x").unwrap_or(result);
    u64::from_str_radix(hex, 16)
        .map_err(|e| format!("parse hex block number: {e}"))
}

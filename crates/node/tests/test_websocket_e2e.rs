//! E2E test: WebSocket subscription channels via TestNode harness
//!
//! Validates that all 9 broadcast subscription channels deliver events
//! end-to-end using RpcModule::raw_json_request.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use std::sync::Arc;

fn parse_subscription_id(resp: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(resp).expect("parse subscribe response");
    if let Some(s) = v["result"].as_str() {
        return s.to_string();
    }
    if let Some(n) = v["result"].as_u64() {
        return n.to_string();
    }
    panic!("no result in response: {}", resp);
}

async fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<String>) -> serde_json::Value {
    let msg = rx.recv().await.expect("should receive event");
    serde_json::from_str(&msg).expect("parse event json")
}

/// `call_subscribeNewBlocks` receives `NewBlock` events.
#[tokio::test]
async fn test_ws_subscribe_new_blocks() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeNewBlocks","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty(), "should get subscription id");

    node.state.subscriptions.broadcast_block(42, "0xabc".into(), 7, 3);

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "new_block");
    assert_eq!(event["params"]["result"]["height"], 42);
    assert_eq!(event["params"]["result"]["tx_count"], 3);
}

/// `call_subscribeNewPayments` receives `NewPayment` events.
#[tokio::test]
async fn test_ws_subscribe_new_payments() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeNewPayments","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state.subscriptions.broadcast_payment(
        "0xdead".into(),
        "0xaaa".into(),
        "0xbbb".into(),
        1,
        500,
    );

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "new_payment");
    assert_eq!(event["params"]["result"]["amount"], 500);
    assert_eq!(event["params"]["result"]["asset_id"], 1);
}

/// `call_subscribeBridgeCompleted` receives `BridgeCompleted` events.
#[tokio::test]
async fn test_ws_subscribe_bridge_completed() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeBridgeCompleted","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state.subscriptions.broadcast_bridge(99, "completed".into());

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "bridge_completed");
    assert_eq!(event["params"]["result"]["op_id"], 99);
    assert_eq!(event["params"]["result"]["status"], "completed");
}

/// `call_subscribeAssetRegistered` receives `AssetRegistered` events.
#[tokio::test]
async fn test_ws_subscribe_asset_registered() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeAssetRegistered","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state
        .subscriptions
        .broadcast_asset(7, "GOLD".into(), "0x1111".into());

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "asset_registered");
    assert_eq!(event["params"]["result"]["asset_id"], 7);
    assert_eq!(event["params"]["result"]["symbol"], "GOLD");
}

/// `call_subscribeAgentExecuted` receives `AgentExecuted` events.
#[tokio::test]
async fn test_ws_subscribe_agent_executed() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeAgentExecuted","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state.subscriptions.broadcast_agent_exec(3, "deploy".into());

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "agent_executed");
    assert_eq!(event["params"]["result"]["agent_id"], 3);
    assert_eq!(event["params"]["result"]["action"], "deploy");
}

/// `call_subscribeAgentRevoked` receives `AgentRevoked` events.
#[tokio::test]
async fn test_ws_subscribe_agent_revoked() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeAgentRevoked","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state.subscriptions.broadcast_agent_revoked(5);

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "agent_revoked");
    assert_eq!(event["params"]["result"]["agent_id"], 5);
}

/// `call_subscribeShieldedDeposit` receives `ShieldedDeposit` events.
#[tokio::test]
async fn test_ws_subscribe_shielded_deposit() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeShieldedDeposit","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state
        .subscriptions
        .broadcast_shielded_deposit("0xcafe".into());

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "shielded_deposit");
    assert_eq!(event["params"]["result"]["commitment"], "0xcafe");
}

/// `call_subscribeShieldedWithdrawal` receives `ShieldedWithdrawal` events.
#[tokio::test]
async fn test_ws_subscribe_shielded_withdrawal() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeShieldedWithdrawal","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state
        .subscriptions
        .broadcast_shielded_withdrawal("0xbead".into());

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "shielded_withdrawal");
    assert_eq!(event["params"]["result"]["nullifier"], "0xbead");
}

/// `call_subscribeGovernance` receives `GovernanceEvent` events.
#[tokio::test]
async fn test_ws_subscribe_governance() {
    let node = TestNode::new();
    let module = call_rpc::build_rpc_module(Arc::clone(&node.state)).expect("build rpc module");

    let (resp, mut rx) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"call_subscribeGovernance","params":[]}"#,
            1024,
        )
        .await
        .expect("raw_json_request should succeed");
    let sub_id = parse_subscription_id(&resp);
    assert!(!sub_id.is_empty());

    node.state.subscriptions.broadcast_governance(
        "proposal_created".into(),
        12,
        "new fee params".into(),
    );

    let event = recv_event(&mut rx).await;
    assert_eq!(event["params"]["result"]["type"], "governance_event");
    assert_eq!(event["params"]["result"]["proposal_id"], 12);
    assert_eq!(event["params"]["result"]["event"], "proposal_created");
}

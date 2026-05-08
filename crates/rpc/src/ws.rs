//! WebSocket subscription support (per spec §11.5)
//!
//! Real subscription manager with broadcast channels for pushing
//! live events to WebSocket clients.

use crate::handlers::RpcState;
use jsonrpsee::server::SubscriptionMessage;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;
use std::sync::Arc;
use tokio::sync::broadcast;

// ── Event Types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    NewBlock {
        height: u64,
        hash: String,
        proposer: u32,
        tx_count: usize,
    },
    NewPayment {
        tx_hash: String,
        from: String,
        to: String,
        asset_id: u64,
        amount: u128,
    },
    BridgeCompleted {
        op_id: u64,
        status: String,
    },
    AssetRegistered {
        asset_id: u64,
        symbol: String,
        issuer: String,
    },
    AgentExecuted {
        agent_id: u64,
        action: String,
    },
    AgentRevoked {
        agent_id: u64,
    },
    ShieldedDeposit {
        commitment: String,
    },
    ShieldedWithdrawal {
        nullifier: String,
    },
    GovernanceEvent {
        event: String,
        proposal_id: u64,
        details: String,
    },
    /// Sent when a subscriber falls behind and missed events.
    Lagged {
        dropped: u64,
    },
}

// ── Subscription Manager ─────────────────────────────────────────────

#[derive(Clone)]
pub struct SubscriptionManager {
    block_tx: broadcast::Sender<WsEvent>,
    payment_tx: broadcast::Sender<WsEvent>,
    bridge_tx: broadcast::Sender<WsEvent>,
    asset_tx: broadcast::Sender<WsEvent>,
    agent_exec_tx: broadcast::Sender<WsEvent>,
    agent_revoked_tx: broadcast::Sender<WsEvent>,
    shielded_deposit_tx: broadcast::Sender<WsEvent>,
    shielded_withdrawal_tx: broadcast::Sender<WsEvent>,
    governance_tx: broadcast::Sender<WsEvent>,
    /// ETH-compatible newHeads subscription channel
    pub eth_new_heads_tx: broadcast::Sender<serde_json::Value>,
    /// ETH-compatible logs subscription channel
    pub eth_logs_tx: broadcast::Sender<serde_json::Value>,
    /// ETH-compatible newPendingTransactions subscription channel
    pub eth_pending_tx_tx: broadcast::Sender<serde_json::Value>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        let (block_tx, _) = broadcast::channel(1024);
        let (payment_tx, _) = broadcast::channel(1024);
        let (bridge_tx, _) = broadcast::channel(256);
        let (asset_tx, _) = broadcast::channel(256);
        let (agent_exec_tx, _) = broadcast::channel(256);
        let (agent_revoked_tx, _) = broadcast::channel(256);
        let (shielded_deposit_tx, _) = broadcast::channel(512);
        let (shielded_withdrawal_tx, _) = broadcast::channel(512);
        let (governance_tx, _) = broadcast::channel(256);
        let (eth_new_heads_tx, _) = broadcast::channel(256);
        let (eth_logs_tx, _) = broadcast::channel(1024);
        let (eth_pending_tx_tx, _) = broadcast::channel(2048);
        Self {
            block_tx,
            payment_tx,
            bridge_tx,
            asset_tx,
            agent_exec_tx,
            agent_revoked_tx,
            shielded_deposit_tx,
            shielded_withdrawal_tx,
            governance_tx,
            eth_new_heads_tx,
            eth_logs_tx,
            eth_pending_tx_tx,
        }
    }

    pub fn broadcast_block(&self, height: u64, hash: String, proposer: u32, tx_count: usize) {
        let _ = self.block_tx.send(WsEvent::NewBlock {
            height,
            hash,
            proposer,
            tx_count,
        });
    }

    pub fn broadcast_payment(
        &self,
        tx_hash: String,
        from: String,
        to: String,
        asset_id: u64,
        amount: u128,
    ) {
        let _ = self.payment_tx.send(WsEvent::NewPayment {
            tx_hash,
            from,
            to,
            asset_id,
            amount,
        });
    }

    pub fn broadcast_bridge(&self, op_id: u64, status: String) {
        let _ = self
            .bridge_tx
            .send(WsEvent::BridgeCompleted { op_id, status });
    }

    pub fn broadcast_asset(&self, asset_id: u64, symbol: String, issuer: String) {
        let _ = self.asset_tx.send(WsEvent::AssetRegistered {
            asset_id,
            symbol,
            issuer,
        });
    }

    pub fn broadcast_agent_exec(&self, agent_id: u64, action: String) {
        let _ = self
            .agent_exec_tx
            .send(WsEvent::AgentExecuted { agent_id, action });
    }

    pub fn broadcast_agent_revoked(&self, agent_id: u64) {
        let _ = self
            .agent_revoked_tx
            .send(WsEvent::AgentRevoked { agent_id });
    }

    pub fn broadcast_shielded_deposit(&self, commitment: String) {
        let _ = self
            .shielded_deposit_tx
            .send(WsEvent::ShieldedDeposit { commitment });
    }

    pub fn broadcast_shielded_withdrawal(&self, nullifier: String) {
        let _ = self
            .shielded_withdrawal_tx
            .send(WsEvent::ShieldedWithdrawal { nullifier });
    }

    pub fn broadcast_governance(&self, event: String, proposal_id: u64, details: String) {
        let _ = self.governance_tx.send(WsEvent::GovernanceEvent {
            event,
            proposal_id,
            details,
        });
    }

    /// Broadcast a new block header for eth_subscribe("newHeads")
    pub fn broadcast_eth_new_head(&self, head: serde_json::Value) {
        let _ = self.eth_new_heads_tx.send(head);
    }

    /// Broadcast a log for eth_subscribe("logs")
    pub fn broadcast_eth_log(&self, log: serde_json::Value) {
        let _ = self.eth_logs_tx.send(log);
    }

    /// Broadcast a pending transaction hash for eth_subscribe("newPendingTransactions")
    pub fn broadcast_eth_pending_tx(&self, tx_hash: String) {
        let _ = self
            .eth_pending_tx_tx
            .send(serde_json::Value::String(tx_hash));
    }

    /// Subscribe to block events (for testing lag handling).
    #[cfg(test)]
    pub fn subscribe_blocks(&self) -> broadcast::Receiver<WsEvent> {
        self.block_tx.subscribe()
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Subscription Registration ────────────────────────────────────────

fn internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg, None::<()>)
}

fn register_subscription(
    module: &mut RpcModule<Arc<RpcState>>,
    subscribe_name: &'static str,
    unsubscribe_name: &'static str,
    tx: broadcast::Sender<WsEvent>,
) -> Result<(), ErrorObjectOwned> {
    module
        .register_subscription(
            subscribe_name,
            "result",
            unsubscribe_name,
            move |_params, pending, _ctx, _conn| {
                let tx = tx.clone();
                async move {
                    let sink = pending
                        .accept()
                        .await
                        .map_err(|e| format!("accept failed: {e}"))?;
                    let mut rx = tx.subscribe();
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                let msg = SubscriptionMessage::from_json(&event)
                                    .map_err(|e| format!("json serialize failed: {e}"))?;
                                if sink.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    subscription = subscribe_name,
                                    lagged = n,
                                    "subscriber lagged, sending lag notification"
                                );
                                let lag_msg =
                                    SubscriptionMessage::from_json(&WsEvent::Lagged { dropped: n })
                                        .map_err(|e| format!("json serialize failed: {e}"))?;
                                if sink.send(lag_msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    Ok(())
                }
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;
    Ok(())
}

/// Register WebSocket subscription methods with the RPC module.
pub fn register_ws_subscriptions(
    module: &mut RpcModule<Arc<RpcState>>,
    subscriptions: &SubscriptionManager,
) -> Result<(), ErrorObjectOwned> {
    register_subscription(
        module,
        "call_subscribeNewBlocks",
        "call_unsubscribeNewBlocks",
        subscriptions.block_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeNewPayments",
        "call_unsubscribeNewPayments",
        subscriptions.payment_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeBridgeCompleted",
        "call_unsubscribeBridgeCompleted",
        subscriptions.bridge_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeAssetRegistered",
        "call_unsubscribeAssetRegistered",
        subscriptions.asset_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeAgentExecuted",
        "call_unsubscribeAgentExecuted",
        subscriptions.agent_exec_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeAgentRevoked",
        "call_unsubscribeAgentRevoked",
        subscriptions.agent_revoked_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeShieldedDeposit",
        "call_unsubscribeShieldedDeposit",
        subscriptions.shielded_deposit_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeShieldedWithdrawal",
        "call_unsubscribeShieldedWithdrawal",
        subscriptions.shielded_withdrawal_tx.clone(),
    )?;
    register_subscription(
        module,
        "call_subscribeGovernance",
        "call_unsubscribeGovernance",
        subscriptions.governance_tx.clone(),
    )?;

    // eth_subscribe / eth_unsubscribe
    module
        .register_subscription(
            "eth_subscribe",
            "eth_subscription",
            "eth_unsubscribe",
            move |params, pending, ctx, _conn| {
                let state = ctx.clone();
                async move {
                    let params_arr: Vec<serde_json::Value> =
                        params.parse().map_err(|e| format!("invalid params: {e}"))?;
                    let sub_type = params_arr
                        .first()
                        .and_then(|v| v.as_str())
                        .ok_or("missing subscription type")?;

                    let sink = pending
                        .accept()
                        .await
                        .map_err(|e| format!("accept failed: {e}"))?;

                    match sub_type {
                        "newHeads" => {
                            let mut rx = state.subscriptions.eth_new_heads_tx.subscribe();
                            loop {
                                match rx.recv().await {
                                    Ok(event) => {
                                        let msg = SubscriptionMessage::from_json(&event)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(
                                            subscription = "eth_subscribe:newHeads",
                                            lagged = n,
                                            "subscriber lagged, sending lag notification"
                                        );
                                        let lag = serde_json::json!({
                                            "subscription": "newHeads",
                                            "lagged": n,
                                            "error": "subscriber lagged",
                                        });
                                        let msg = SubscriptionMessage::from_json(&lag)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                        "logs" => {
                            let filter = params_arr.get(1).cloned();
                            let addresses: Vec<String> = filter
                                .as_ref()
                                .and_then(|f| f.get("address"))
                                .map(|v| match v {
                                    serde_json::Value::String(s) => vec![s.to_lowercase()],
                                    serde_json::Value::Array(arr) => arr
                                        .iter()
                                        .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
                                        .collect(),
                                    _ => vec![],
                                })
                                .unwrap_or_default();
                            let topics_filter: Vec<Option<Vec<String>>> = filter
                                .as_ref()
                                .and_then(|f| f.get("topics"))
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .map(|entry| match entry {
                                            serde_json::Value::String(s) => {
                                                Some(vec![s.to_lowercase()])
                                            }
                                            serde_json::Value::Array(arr) => {
                                                let vals: Vec<_> = arr
                                                    .iter()
                                                    .filter_map(|x| {
                                                        x.as_str().map(|s| s.to_lowercase())
                                                    })
                                                    .collect();
                                                if vals.is_empty() {
                                                    None
                                                } else {
                                                    Some(vals)
                                                }
                                            }
                                            serde_json::Value::Null => None,
                                            _ => None,
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();

                            let mut rx = state.subscriptions.eth_logs_tx.subscribe();
                            loop {
                                match rx.recv().await {
                                    Ok(log) => {
                                        let log_address = log
                                            .get("address")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_lowercase())
                                            .unwrap_or_default();
                                        if !addresses.is_empty()
                                            && !addresses.contains(&log_address)
                                        {
                                            continue;
                                        }
                                        if !topics_filter.is_empty() {
                                            let log_topics: Vec<String> = log
                                                .get("topics")
                                                .and_then(|v| v.as_array())
                                                .map(|arr| {
                                                    arr.iter()
                                                        .filter_map(|x| {
                                                            x.as_str().map(|s| s.to_lowercase())
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            let mut matched = true;
                                            for (idx, topic_filter) in
                                                topics_filter.iter().enumerate()
                                            {
                                                if let Some(filter_vals) = topic_filter {
                                                    if let Some(log_topic) = log_topics.get(idx) {
                                                        if !filter_vals.contains(log_topic) {
                                                            matched = false;
                                                            break;
                                                        }
                                                    } else {
                                                        matched = false;
                                                        break;
                                                    }
                                                }
                                            }
                                            if !matched {
                                                continue;
                                            }
                                        }
                                        let msg = SubscriptionMessage::from_json(&log)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(
                                            subscription = "eth_subscribe:logs",
                                            lagged = n,
                                            "subscriber lagged, sending lag notification"
                                        );
                                        let lag = serde_json::json!({
                                            "subscription": "logs",
                                            "lagged": n,
                                            "error": "subscriber lagged",
                                        });
                                        let msg = SubscriptionMessage::from_json(&lag)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                        "newPendingTransactions" => {
                            let mut rx = state.subscriptions.eth_pending_tx_tx.subscribe();
                            loop {
                                match rx.recv().await {
                                    Ok(event) => {
                                        let msg = SubscriptionMessage::from_json(&event)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(
                                            subscription = "eth_subscribe:newPendingTransactions",
                                            lagged = n,
                                            "subscriber lagged, sending lag notification"
                                        );
                                        let lag = serde_json::json!({
                                            "subscription": "newPendingTransactions",
                                            "lagged": n,
                                            "error": "subscriber lagged",
                                        });
                                        let msg = SubscriptionMessage::from_json(&lag)
                                            .map_err(|e| format!("json serialize failed: {e}"))?;
                                        if sink.send(msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                        other => {
                            tracing::warn!(sub_type = other, "unsupported eth_subscribe type");
                        }
                    }
                    Ok(())
                }
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;

    Ok(())
}

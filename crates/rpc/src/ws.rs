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
    NewBlock { height: u64, hash: String, proposer: u32, tx_count: usize },
    NewPayment { tx_hash: String, from: String, to: String, asset_id: u64, amount: u128 },
    BridgeCompleted { op_id: u64, status: String },
    AssetRegistered { asset_id: u64, symbol: String, issuer: String },
    AgentExecuted { agent_id: u64, action: String },
    AgentRevoked { agent_id: u64 },
    ShieldedDeposit { commitment: String },
    ShieldedWithdrawal { nullifier: String },
    GovernanceEvent { event: String, proposal_id: u64, details: String },
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
        Self {
            block_tx, payment_tx, bridge_tx, asset_tx,
            agent_exec_tx, agent_revoked_tx,
            shielded_deposit_tx, shielded_withdrawal_tx,
            governance_tx,
        }
    }

    pub fn broadcast_block(&self, height: u64, hash: String, proposer: u32, tx_count: usize) {
        let _ = self.block_tx.send(WsEvent::NewBlock { height, hash, proposer, tx_count });
    }

    pub fn broadcast_payment(&self, tx_hash: String, from: String, to: String, asset_id: u64, amount: u128) {
        let _ = self.payment_tx.send(WsEvent::NewPayment { tx_hash, from, to, asset_id, amount });
    }

    pub fn broadcast_bridge(&self, op_id: u64, status: String) {
        let _ = self.bridge_tx.send(WsEvent::BridgeCompleted { op_id, status });
    }

    pub fn broadcast_asset(&self, asset_id: u64, symbol: String, issuer: String) {
        let _ = self.asset_tx.send(WsEvent::AssetRegistered { asset_id, symbol, issuer });
    }

    pub fn broadcast_agent_exec(&self, agent_id: u64, action: String) {
        let _ = self.agent_exec_tx.send(WsEvent::AgentExecuted { agent_id, action });
    }

    pub fn broadcast_agent_revoked(&self, agent_id: u64) {
        let _ = self.agent_revoked_tx.send(WsEvent::AgentRevoked { agent_id });
    }

    pub fn broadcast_shielded_deposit(&self, commitment: String) {
        let _ = self.shielded_deposit_tx.send(WsEvent::ShieldedDeposit { commitment });
    }

    pub fn broadcast_shielded_withdrawal(&self, nullifier: String) {
        let _ = self.shielded_withdrawal_tx.send(WsEvent::ShieldedWithdrawal { nullifier });
    }

    pub fn broadcast_governance(&self, event: String, proposal_id: u64, details: String) {
        let _ = self.governance_tx.send(WsEvent::GovernanceEvent { event, proposal_id, details });
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
                    let sink = pending.accept().await.map_err(|e| format!("accept failed: {e}"))?;
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
                                tracing::warn!(subscription = subscribe_name, lagged = n, "subscriber lagged");
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
    register_subscription(module, "call_subscribeNewBlocks", "call_unsubscribeNewBlocks", subscriptions.block_tx.clone())?;
    register_subscription(module, "call_subscribeNewPayments", "call_unsubscribeNewPayments", subscriptions.payment_tx.clone())?;
    register_subscription(module, "call_subscribeBridgeCompleted", "call_unsubscribeBridgeCompleted", subscriptions.bridge_tx.clone())?;
    register_subscription(module, "call_subscribeAssetRegistered", "call_unsubscribeAssetRegistered", subscriptions.asset_tx.clone())?;
    register_subscription(module, "call_subscribeAgentExecuted", "call_unsubscribeAgentExecuted", subscriptions.agent_exec_tx.clone())?;
    register_subscription(module, "call_subscribeAgentRevoked", "call_unsubscribeAgentRevoked", subscriptions.agent_revoked_tx.clone())?;
    register_subscription(module, "call_subscribeShieldedDeposit", "call_unsubscribeShieldedDeposit", subscriptions.shielded_deposit_tx.clone())?;
    register_subscription(module, "call_subscribeShieldedWithdrawal", "call_unsubscribeShieldedWithdrawal", subscriptions.shielded_withdrawal_tx.clone())?;
    register_subscription(module, "call_subscribeGovernance", "call_unsubscribeGovernance", subscriptions.governance_tx.clone())?;
    Ok(())
}

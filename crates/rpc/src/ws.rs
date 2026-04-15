//! WebSocket subscription support (per spec §11.5)

use crate::handlers::RpcState;
use jsonrpsee::server::SubscriptionMessage;
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::Arc;

fn internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg, None::<()>)
}

/// Register WebSocket subscription methods
pub fn register_ws_subscriptions(module: &mut RpcModule<Arc<RpcState>>) -> Result<(), ErrorObjectOwned> {
    register_sub(module, "call_subscribeNewPaymentBlock", "call_unsubscribeNewPaymentBlock")?;
    register_sub(module, "call_subscribePaymentReceived", "call_unsubscribePaymentReceived")?;
    register_sub(module, "call_subscribeBridgeCompleted", "call_unsubscribeBridgeCompleted")?;
    register_sub(module, "call_subscribeAssetRegistered", "call_unsubscribeAssetRegistered")?;
    register_sub(module, "call_subscribeAgentExecuted", "call_unsubscribeAgentExecuted")?;
    register_sub(module, "call_subscribeAgentRevoked", "call_unsubscribeAgentRevoked")?;
    register_sub(module, "call_subscribeShieldedDeposit", "call_unsubscribeShieldedDeposit")?;
    register_sub(module, "call_subscribeShieldedWithdrawal", "call_unsubscribeShieldedWithdrawal")?;
    Ok(())
}

fn register_sub(
    module: &mut RpcModule<Arc<RpcState>>,
    subscribe_name: &'static str,
    unsubscribe_name: &'static str,
) -> Result<(), ErrorObjectOwned> {
    module
        .register_subscription(
            subscribe_name,
            "result",
            unsubscribe_name,
            move |_params, pending, _ctx, _conn| async move {
                let sink = pending.accept().await.map_err(|e| format!("accept failed: {}", e))?;
                let msg = SubscriptionMessage::from_json(&serde_json::json!({
                    "subscription": subscribe_name,
                    "status": "subscribed"
                })).map_err(|e| format!("json serialize failed: {}", e))?;
                let _ = sink.send(msg);
                Ok(())
            },
        )
        .map_err(|e| internal_error(e.to_string()))?;
    Ok(())
}

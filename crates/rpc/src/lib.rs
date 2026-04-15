//! call-rpc — JSON-RPC server (per spec §11)

pub mod handlers;
pub mod standard;
pub mod callchain;
pub mod ws;

#[cfg(test)]
mod tests;

pub use handlers::*;
pub use standard::*;
pub use callchain::*;
pub use ws::*;

use jsonrpsee::server::{Server, ServerHandle};
use jsonrpsee::RpcModule;
use jsonrpsee::types::ErrorObjectOwned;
use std::net::SocketAddr;
use std::sync::Arc;

/// RPC server configuration
#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub http_addr: SocketAddr,
    pub ws_addr: SocketAddr,
    pub max_connections: u32,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            http_addr: "127.0.0.1:8545".parse().unwrap(),
            ws_addr: "127.0.0.1:8546".parse().unwrap(),
            max_connections: 100,
        }
    }
}

/// Start the HTTP JSON-RPC server
pub async fn start_http_server(
    config: RpcConfig,
    module: RpcModule<()>,
) -> Result<ServerHandle, ErrorObjectOwned> {
    let server = Server::builder()
        .max_connections(config.max_connections)
        .build(config.http_addr)
        .await
        .map_err(|e| ErrorObjectOwned::owned(-32603, format!("bind failed: {}", e), None::<()>))?;
    let handle = server.start(module);
    tracing::info!("HTTP RPC server started on {}", config.http_addr);
    Ok(handle)
}

/// Build a combined RPC module with all endpoints
pub fn build_rpc_module(state: Arc<RpcState>) -> Result<RpcModule<Arc<RpcState>>, ErrorObjectOwned> {
    let mut module = RpcModule::new(state);
    standard::register_standard_rpc(&mut module)?;
    callchain::register_callchain_rpc(&mut module)?;
    ws::register_ws_subscriptions(&mut module)?;
    Ok(module)
}

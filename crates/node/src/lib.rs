//! call-node — Callchain node application.
//!
//! Minimal node that initializes all state components,
//! starts an HTTP RPC server, and processes transactions.

use call_rpc::{RpcState, RpcConfig, build_rpc_module};
use call_transaction_pool::Mempool;
use call_protocol::{BalanceState, AssetRegistry, ComplianceEngine};
use call_evm::EvmState;
use call_bridge::BridgeStateManager;
use call_consensus::ValidatorStateManager;
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use jsonrpsee::server::{Server, ServerHandle};
use std::sync::{Arc, RwLock};

/// Chain ID for Callchain devnet
pub const CALLCHAIN_CHAIN_ID: u64 = 1337;

/// The Callchain node
pub struct CallNode {
    pub state: Arc<RpcState>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub server_handle: Option<ServerHandle>,
}

impl CallNode {
    /// Create a new node with default state
    pub fn new() -> Self {
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        let state = Arc::new(RpcState::new(
            BalanceState::new(),
            AssetRegistry::new(),
            ComplianceEngine::new(),
            EvmState::new(),
            BridgeStateManager::default(),
            ValidatorStateManager::default(),
            AgentRegistry::new(),
            AgentBalances::new(),
            ShieldedState::new(),
            mempool.clone(),
            CALLCHAIN_CHAIN_ID,
        ));
        Self {
            state,
            mempool,
            server_handle: None,
        }
    }

    /// Start the HTTP RPC server
    pub async fn start_rpc(&mut self, config: RpcConfig) -> Result<(), String> {
        let module = build_rpc_module(Arc::clone(&self.state))
            .map_err(|e| format!("failed to build RPC module: {e}"))?;

        let server = Server::builder()
            .max_connections(config.max_connections)
            .build(config.http_addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;

        let handle = server.start(module);
        tracing::info!("HTTP RPC server started on {}", config.http_addr);

        self.server_handle = Some(handle);
        Ok(())
    }

    /// Stop the RPC server
    pub async fn stop(&mut self) -> Result<(), String> {
        if let Some(handle) = self.server_handle.take() {
            handle.stop().map_err(|_| "server already stopped".to_string())?;
        }
        Ok(())
    }

    /// Get mempool stats
    pub fn mempool_stats(&self) -> (usize, usize, usize) {
        let mempool = self.mempool.read().unwrap();
        (
            mempool.protocol_pool.len(),
            mempool.evm_pool.len(),
            mempool.pending_bridges.len(),
        )
    }
}

impl Default for CallNode {
    fn default() -> Self {
        Self::new()
    }
}

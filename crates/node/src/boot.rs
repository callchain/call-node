//! T14.1 — Boot sequence (per spec §21.3)
//!
//! parse config → init logging → open DB → load genesis → init P2P →
//! connect seeds → init consensus → start RPC → sync/participate

use crate::config::{NodeConfig, NodeMode, parse_bootstrap_peers};
use crate::CallNode;
use call_network::CommonwareConfig;
use call_primitives::Address;
use call_rpc::RpcConfig;
use serde::Deserialize;
use std::fs;
use tracing::info;

/// Result type for boot sequence
pub type BootResult = Result<CallNode, String>;

/// Genesis file format: balances, assets, validators, timestamp.
#[derive(Debug, Clone, Deserialize)]
pub struct Genesis {
    #[serde(default)]
    pub balances: Vec<GenesisBalance>,
    #[serde(default)]
    pub validators: Vec<GenesisValidator>,
    #[serde(default = "Genesis::default_timestamp")]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisBalance {
    pub address: String,
    pub asset_id: u64,
    pub amount: u128,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisValidator {
    pub address: String,
    pub pubkey: String,
    pub stake: u128,
}

impl Genesis {
    fn default_timestamp() -> u64 {
        1_000_000 // default genesis timestamp
    }

    /// Load and parse a genesis file from disk
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read genesis file: {e}"))?;
        let genesis: Genesis = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse genesis JSON: {e}"))?;
        Ok(genesis)
    }

    /// Apply genesis state to the node: balances and validators
    pub fn apply(&self, node: &mut CallNode) -> Result<(), String> {
        // Apply genesis balances
        let mut balance_state = node.state.balance_state.write().map_err(|_| "lock poisoned")?;
        for entry in &self.balances {
            let addr = parse_address(&entry.address)?;
            balance_state
                .balances
                .set_balance(entry.asset_id, addr, entry.amount)
                .map_err(|e| format!("failed to set genesis balance: {e}"))?;
        }

        // Stake genesis validators
        let mut consensus = node.consensus.write().map_err(|_| "lock poisoned")?;
        for val in &self.validators {
            let addr = parse_address(&val.address)?;
            let pubkey = parse_pubkey(&val.pubkey)?;
            consensus
                .stake_validator(addr, pubkey, val.stake)
                .map_err(|e| format!("failed to stake validator: {e}"))?;
        }
        consensus.refresh_proposer_subset();

        Ok(())
    }
}

fn parse_address(s: &str) -> Result<Address, String> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| format!("invalid address hex: {e}"))?;
    if bytes.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", bytes.len()));
    }
    Ok(Address::from_slice(&bytes))
}

fn parse_pubkey(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| format!("invalid pubkey hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("pubkey must be 32 bytes, got {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Execute the full boot sequence per §21.3.
pub async fn boot_node(config: &NodeConfig) -> BootResult {
    // Step 1: Open DB (resume from existing data if present)
    info!(data_dir = ?config.storage.data_dir, "step 1: opening database");
    let existing_data = config.storage.data_dir.join("blocks").exists();
    if existing_data {
        info!("existing data found — resuming from last saved state");
    } else {
        info!("fresh database — will initialize from genesis");
    }

    // Step 2: Create node (opens DB, initializes state)
    info!("step 2: initializing node");
    let mut node = CallNode::new(config.storage.data_dir.clone())?;

    // Step 3: Load genesis if path provided
    if let Some(ref genesis_path) = config.genesis.path {
        info!(path = ?genesis_path, "step 3: loading genesis");
        let genesis = Genesis::load(genesis_path)?;
        info!(
            balances = genesis.balances.len(),
            validators = genesis.validators.len(),
            "genesis loaded"
        );
        genesis.apply(&mut node)?;
    }

    // Step 4: Init P2P and connect seeds
    if config.p2p.bootstrap_peers.is_some() || config.p2p.bootstrap_peers.is_none() {
        info!(listen = %config.p2p.listen_addr, "step 4: initializing P2P");
        let bootstrap = parse_bootstrap_peers(&config.p2p.bootstrap_peers);
        let p2p_config = CommonwareConfig {
            listen_addr: config.p2p.listen_addr,
            bootstrap_peers: bootstrap,
            max_message_size: 10 * 1024 * 1024,
            allow_private_ips: true,
            namespace: b"callchain".to_vec(),
        };
        node.start_network(p2p_config).await?;
    }

    // Step 5: Init consensus (validator or full node)
    match config.mode {
        NodeMode::Validator => {
            info!("step 5: starting in validator mode");
        }
        NodeMode::Full => {
            info!("step 5: starting in full node mode");
        }
        NodeMode::Archive => {
            info!("step 5: starting in archive node mode");
        }
    }

    // Step 6: Start RPC
    info!(http = %config.rpc.http_addr, ws = %config.rpc.ws_addr, "step 6: starting RPC");
    let rpc_config = RpcConfig {
        http_addr: config.rpc.http_addr,
        ws_addr: config.rpc.ws_addr,
        max_connections: config.rpc.max_connections,
    };
    node.start_rpc(rpc_config.clone()).await?;
    node.start_ws_rpc(rpc_config).await?;

    // Step 7: Start consensus block production
    info!("step 7: starting consensus loop");
    let _handle = node.start_consensus_loop();

    info!(
        mode = ?config.mode,
        http = %config.rpc.http_addr,
        ws = %config.rpc.ws_addr,
        p2p = %config.p2p.listen_addr,
        metrics = %config.metrics.addr,
        "node booted successfully"
    );

    Ok(node)
}

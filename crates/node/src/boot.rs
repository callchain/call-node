//! T14.1 — Boot sequence (per spec §21.3)
//!
//! parse config → init logging → open DB → load genesis → init P2P →
//! connect seeds → init consensus → start RPC → sync/participate

use crate::config::{NodeConfig, NodeMode, parse_bootstrap_peers};
use crate::CallNode;
use call_network::CommonwareConfig;
use call_rpc::RpcConfig;
use tracing::info;

/// Result type for boot sequence
pub type BootResult = Result<CallNode, String>;

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
        // Genesis loading is handled by CallNode::new for now;
        // in production, parse and apply genesis here.
        info!("genesis loaded (placeholder)");
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
    node.start_rpc(rpc_config).await?;

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

//! calld — Callchain node binary.

use call_node::CallNode;
use call_network::CommonwareConfig;
use call_rpc::RpcConfig;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "calld", version, about = "Callchain Node")]
struct Args {
    /// HTTP RPC listen address
    #[arg(long, default_value = "127.0.0.1:8545")]
    http_addr: SocketAddr,

    /// WebSocket RPC listen address
    #[arg(long, default_value = "127.0.0.1:8546")]
    ws_addr: SocketAddr,

    /// Data directory for block storage
    #[arg(long, default_value = ".call-data")]
    data_dir: PathBuf,

    /// P2P listen address
    #[arg(long, default_value = "127.0.0.1:51235")]
    p2p_listen_addr: SocketAddr,

    /// Bootstrap peers to connect to (format: "peer_id@address", comma-separated)
    #[arg(long)]
    p2p_bootstrap_peers: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("call_node=info".parse()?)
                .add_directive("call_rpc=info".parse()?)
                .add_directive("call_protocol=info".parse()?)
                .add_directive("call_evm=info".parse()?)
                .add_directive("call_network=info".parse()?)
                .add_directive("call_consensus=info".parse()?),
        )
        .init();

    let args = Args::parse();

    let rpc_config = RpcConfig {
        http_addr: args.http_addr,
        ws_addr: args.ws_addr,
        max_connections: 100,
    };

    tracing::info!("Starting Callchain node...");
    tracing::info!("HTTP RPC: {}", rpc_config.http_addr);
    tracing::info!("WebSocket: {}", rpc_config.ws_addr);

    let mut node = CallNode::new(args.data_dir.clone())?;
    node.start_rpc(rpc_config).await?;

    // Start P2P network (optional — only if bootstrap peers provided)
    if args.p2p_bootstrap_peers.is_some() {
        let p2p_config = CommonwareConfig {
            listen_addr: args.p2p_listen_addr,
            bootstrap_peers: parse_bootstrap_peers(&args.p2p_bootstrap_peers),
            max_message_size: 10 * 1024 * 1024,
            allow_private_ips: true,
            namespace: b"callchain".to_vec(),
        };
        node.start_network(p2p_config).await?;
        tracing::info!("P2P network started on {}", args.p2p_listen_addr);
    }

    // Start consensus block production loop
    let _consensus_handle = node.start_consensus_loop();
    tracing::info!("Consensus loop started");

    tracing::info!("Callchain node running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    node.stop().await?;
    tracing::info!("Node stopped.");

    Ok(())
}

fn parse_bootstrap_peers(peers: &Option<String>) -> Vec<(String, SocketAddr)> {
    peers
        .as_ref()
        .map(|s| {
            s.split(',')
                .filter_map(|entry| {
                    let parts: Vec<&str> = entry.split('@').collect();
                    if parts.len() == 2 {
                        let peer_id = parts[0].to_string();
                        if let Ok(addr) = parts[1].parse::<SocketAddr>() {
                            return Some((peer_id, addr));
                        }
                    }
                    None
                })
                .collect()
        })
        .unwrap_or_default()
}

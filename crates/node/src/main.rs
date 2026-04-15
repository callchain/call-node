//! calld — Callchain node binary.

use call_node::CallNode;
use call_rpc::RpcConfig;
use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(name = "calld", version, about = "Callchain Node")]
struct Args {
    /// HTTP RPC listen address
    #[arg(long, default_value = "127.0.0.1:8545")]
    http_addr: SocketAddr,

    /// WebSocket RPC listen address
    #[arg(long, default_value = "127.0.0.1:8546")]
    ws_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("call_node=info".parse()?)
                .add_directive("call_rpc=info".parse()?)
                .add_directive("call_protocol=info".parse()?)
                .add_directive("call_evm=info".parse()?),
        )
        .init();

    let args = Args::parse();

    let config = RpcConfig {
        http_addr: args.http_addr,
        ws_addr: args.ws_addr,
        max_connections: 100,
    };

    tracing::info!("Starting Callchain node...");
    tracing::info!("HTTP RPC: {}", config.http_addr);
    tracing::info!("WebSocket: {}", config.ws_addr);

    let mut node = CallNode::new();
    node.start_rpc(config).await?;

    tracing::info!("Callchain node running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    node.stop().await?;
    tracing::info!("Node stopped.");

    Ok(())
}

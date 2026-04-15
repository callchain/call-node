//! calld — Callchain node binary (per spec §21).
//!
//! Boot sequence: parse config → init logging → open DB → load genesis →
//! init P2P → connect seeds → init consensus → start RPC → sync/participate.

use call_node::boot::boot_node;
use call_node::cli::CliArgs;
use call_node::config::NodeConfig;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = CliArgs::parse();

    // Load config: TOML file (if provided) → merge CLI overrides
    let config = match &args.config {
        Some(config_path) => {
            let base = NodeConfig::from_file(config_path)?;
            base.merge_from_cli(&args)
        }
        None => NodeConfig::default().merge_from_cli(&args),
    };
    config.validate()?;

    // Init logging (per config)
    init_logging(&config);

    tracing::info!("Starting Callchain node...");
    tracing::info!("  Mode:      {:?}", config.mode);
    tracing::info!("  HTTP RPC:  {}", config.rpc.http_addr);
    tracing::info!("  WS RPC:    {}", config.rpc.ws_addr);
    tracing::info!("  P2P:       {}", config.p2p.listen_addr);
    tracing::info!("  Metrics:   {}", config.metrics.addr);
    tracing::info!("  Data dir:  {:?}", config.storage.data_dir);
    tracing::info!("  Log level: {}", config.logging.level);

    // Boot sequence
    let mut node = boot_node(&config).await?;

    tracing::info!("Callchain node running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    node.stop().await?;
    tracing::info!("Node stopped.");

    Ok(())
}

/// Initialize logging based on config (per spec §21.3)
fn init_logging(config: &NodeConfig) {
    let level = &config.logging.level;
    let filter = match level.as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "info" => "info",
        "warn" => "warn",
        "error" => "error",
        _ => "info",
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(filter.parse().unwrap()),
        );

    match config.logging.format.as_str() {
        "json" => subscriber.json().init(),
        _ => subscriber.init(),
    }
}

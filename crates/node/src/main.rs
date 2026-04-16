//! calld — Callchain node binary (per spec §21).
//!
//! Boot sequence: parse config → init logging → open DB → load genesis →
//! init P2P → connect seeds → init consensus → start RPC → start metrics → sync/participate.

use call_node::boot::boot_node;
use call_node::cli::{CliArgs, Commands, WalletCommand};
use call_node::config::NodeConfig;
use call_node::telemetry::{init_opentelemetry_tracing, start_metrics_server, TelemetryRegistry};
use call_node::wallet;
use clap::Parser;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = CliArgs::parse();

    // Handle subcommands first
    if let Some(command) = &args.command {
        return handle_command(command).await;
    }

    // Load config: TOML file (if provided) → merge CLI overrides
    let config = match &args.config {
        Some(config_path) => {
            let base = NodeConfig::from_file(config_path)?;
            base.merge_from_cli(&args)
        }
        None => NodeConfig::default().merge_from_cli(&args),
    };
    config.validate()?;

    // Init logging with OpenTelemetry tracing (per spec §21.3 + §20)
    init_logging(&config)?;

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

    // Start Prometheus /metrics HTTP server (per spec §20)
    let registry = Arc::new(TelemetryRegistry::new(config.storage.data_dir.clone()));
    let metrics_addr = start_metrics_server(Arc::clone(&registry), config.metrics.addr).await?;
    tracing::info!("  Metrics server started on http://{metrics_addr}");

    tracing::info!("Callchain node running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    node.stop().await?;
    tracing::info!("Node stopped.");

    // Shut down OpenTelemetry tracer
    if let Some(provider) = call_node::telemetry::GLOBAL_PROVIDER.get() {
        let _ = provider.shutdown();
    }

    Ok(())
}

/// Handle CLI subcommands
async fn handle_command(command: &Commands) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match command {
        Commands::Run { .. } => {
            // Fall through to normal boot (should not reach here due to earlier check)
            Ok(())
        }
        Commands::Wallet(wallet_cmd) => handle_wallet(wallet_cmd).await,
    }
}

async fn handle_wallet(cmd: &WalletCommand) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match cmd {
        WalletCommand::GenerateKeys => wallet::generate_keys(),
        WalletCommand::Address { pubkey } => wallet::derive_address(pubkey),
        WalletCommand::Balance { address, asset_id, rpc_url } => {
            wallet::query_balance(address, *asset_id, rpc_url).await
        }
        WalletCommand::Send { from_key, to, asset_id, amount, nonce, rpc_url } => {
            wallet::send_payment(from_key, to, *asset_id, *amount, *nonce, rpc_url).await
        }
        WalletCommand::ServerInfo { rpc_url } => wallet::server_info(rpc_url).await,
        WalletCommand::Mempool { rpc_url } => wallet::mempool_stats(rpc_url).await,
    }
}

/// Initialize logging with OpenTelemetry tracing integration (per spec §21.3 + §20)
fn init_logging(config: &NodeConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let level = &config.logging.level;
    init_opentelemetry_tracing("call-node", level)
}

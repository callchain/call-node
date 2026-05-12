//! calld — Callchain node binary (per spec §21).
//!
//! Boot sequence: parse config → init logging → open DB → load genesis →
//! init P2P → connect seeds → init consensus → start RPC → start metrics → sync/participate.

use call_node::boot::boot_node;
use call_node::cli::{CliArgs, Commands, WalletCommand};
use call_node::config::NodeConfig;
use call_node::telemetry::{
    init_opentelemetry_tracing_with_file, start_alert_task, start_metrics_server, AlertDispatcher,
    HealthState,
};
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
    if config.solo {
        tracing::info!("  Mode:      {:?} (solo)", config.mode);
    } else {
        tracing::info!("  Mode:      {:?}", config.mode);
    }
    tracing::info!("  HTTP RPC:  {}", config.rpc.http_addr);
    tracing::info!("  WS RPC:    {}", config.rpc.ws_addr);
    tracing::info!("  P2P:       {}", config.p2p.listen_addr);
    tracing::info!("  Metrics:   {}", config.metrics.addr);
    tracing::info!("  Data dir:  {:?}", config.storage.data_dir);
    tracing::info!("  Log level: {}", config.logging.level);

    // Boot sequence
    let mut node = boot_node(&config).await?;

    // Start Prometheus /metrics HTTP server (per spec §20)
    let registry = Arc::clone(&node.telemetry);
    let health = HealthState {
        db: Arc::clone(&node.db.db),
        network: node.network.clone(),
        consensus: Arc::clone(&node.consensus),
    };
    let metrics_addr =
        start_metrics_server(Arc::clone(&registry), health, config.metrics.addr).await?;
    tracing::info!("  Metrics server started on http://{metrics_addr}");

    // Start background alert evaluation task
    let dispatcher = AlertDispatcher::new(None, None);
    let _alert_handle = start_alert_task(registry, dispatcher);
    tracing::info!("  Alert evaluation task started");

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
async fn handle_command(
    command: &Commands,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match command {
        Commands::Run { .. } => {
            // Fall through to normal boot (should not reach here due to earlier check)
            Ok(())
        }
        Commands::Wallet(wallet_cmd) => handle_wallet(wallet_cmd).await,
    }
}

async fn handle_wallet(
    cmd: &WalletCommand,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match cmd {
        WalletCommand::GenerateKeys => wallet::generate_keys(),
        WalletCommand::Address { pubkey } => wallet::derive_address(pubkey),
        WalletCommand::Balance {
            address,
            asset_id,
            rpc_url,
        } => wallet::query_balance(address, *asset_id, rpc_url).await,
        WalletCommand::Send {
            from_key,
            to,
            asset_id,
            amount,
            nonce,
            rpc_url,
        } => wallet::send_payment(from_key, to, *asset_id, *amount, *nonce, rpc_url).await,
        WalletCommand::ServerInfo { rpc_url } => wallet::server_info(rpc_url).await,
        WalletCommand::Mempool { rpc_url } => wallet::mempool_stats(rpc_url).await,
        WalletCommand::StoreKeyring { key, service, user } => {
            #[cfg(feature = "keyring")]
            {
                call_crypto::KeyringSigner::store_key(service, user, key)
                    .map_err(|e| format!("failed to store key in keyring: {e}"))?;
                println!("Key stored successfully in OS keyring (service={service}, user={user})");
                Ok(())
            }
            #[cfg(not(feature = "keyring"))]
            {
                let _ = (key, service, user);
                Err("keyring support not compiled in (enable keyring feature)".into())
            }
        }
    }
}

/// Initialize logging with OpenTelemetry tracing + optional file layer (per spec §21.3 + §20)
fn init_logging(config: &NodeConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log_level = &config.logging.level;

    let log_format = match config.logging.format.as_str() {
        "json" => call_node::logging::LogFormat::Json,
        _ => call_node::logging::LogFormat::Text,
    };

    let log_path = config.storage.data_dir.join("logs").join("node.log");
    let log_config = call_node::logging::LogConfig {
        level: match log_level.as_str() {
            "trace" => call_node::logging::LogLevel::Trace,
            "debug" => call_node::logging::LogLevel::Debug,
            "warn" => call_node::logging::LogLevel::Warn,
            "error" => call_node::logging::LogLevel::Error,
            _ => call_node::logging::LogLevel::Info,
        },
        format: log_format,
        output: call_node::logging::LogOutput::File(log_path),
        rotation: call_node::logging::LogRotation::Size(100 * 1024 * 1024),
        retention_days: 30,
        audit_enabled: true,
        audit_path: config.storage.data_dir.join("audit.log"),
    };

    let file_layer = call_node::logging::file_log::FileLogLayer::new(log_config.clone()).ok();

    init_opentelemetry_tracing_with_file("call-node", log_level, file_layer)
}

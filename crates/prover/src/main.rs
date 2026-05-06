//! call-prover — Shielded ZK proving service.
//!
//! A dedicated HTTP service that generates Groth16 proofs for shielded
//! transactions (deposit, transfer, withdraw). Runs independently of the
//! node to keep private keys off the node and allow CPU scaling.
//!
//! Usage:
//!   call-prover                       # default port 8550
//!   call-prover --listen-addr 0.0.0.0:8550
//!   call-prover --api-keys key1,key2  # require X-API-Key header

mod server;

use call_shielded::prover::RealProver;
use clap::Parser;
use server::{build_router, ProverMode, ProverState};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{atomic::AtomicUsize, Arc, Mutex},
};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "call-prover", about = "Shielded ZK proving service")]
struct CliArgs {
    /// HTTP listen address
    #[arg(long, default_value = "127.0.0.1:8550")]
    listen_addr: SocketAddr,

    /// Comma-separated list of valid API keys. If unset, all requests are allowed.
    #[arg(long, value_delimiter = ',')]
    api_keys: Vec<String>,

    /// Max requests per second per API key (token bucket)
    #[arg(long, default_value = "10.0")]
    max_qps: f64,

    /// Proof cache TTL in seconds
    #[arg(long, default_value = "300")]
    cache_ttl_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = CliArgs::parse();

    // Init logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Initialize prover — uses production keys if available, falls back to dev setup
    let prover = RealProver::global();
    let mode = ProverMode::Dev; // RealProver::global() handles production fallback internally

    let api_keys: HashSet<String> = args.api_keys.into_iter().collect();
    let auth_enabled = !api_keys.is_empty();

    let state = ProverState {
        prover,
        mode,
        api_keys: Arc::new(api_keys),
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
        max_qps: args.max_qps.max(0.1),
        proof_cache: Arc::new(Mutex::new(HashMap::new())),
        cache_ttl_secs: args.cache_ttl_secs,
        inflight: Arc::new(AtomicUsize::new(0)),
    };

    // Build and start server
    let app = build_router(state);

    info!(
        addr = %args.listen_addr,
        mode = mode.as_str(),
        auth_enabled = auth_enabled,
        max_qps = args.max_qps,
        "shielded prover service starting"
    );

    let listener = tokio::net::TcpListener::bind(args.listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

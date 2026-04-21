//! call-prover — Shielded ZK proving service.
//!
//! A dedicated HTTP service that generates Groth16 proofs for shielded
//! transactions (deposit, transfer, withdraw). Runs independently of the
//! node to keep private keys off the node and allow CPU scaling.
//!
//! Usage:
//!   call-prover                       # default port 8550
//!   call-prover --listen-addr 0.0.0.0:8550

mod server;

use server::{ProverMode, ProverState, build_router};
use call_shielded::prover::RealProver;
use clap::Parser;
use std::net::SocketAddr;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "call-prover", about = "Shielded ZK proving service")]
struct CliArgs {
    /// HTTP listen address
    #[arg(long, default_value = "127.0.0.1:8550")]
    listen_addr: SocketAddr,
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
    let state = ProverState { prover, mode };

    // Build and start server
    let app = build_router(state);

    info!(
        addr = %args.listen_addr,
        mode = mode.as_str(),
        "shielded prover service starting"
    );

    let listener = tokio::net::TcpListener::bind(args.listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

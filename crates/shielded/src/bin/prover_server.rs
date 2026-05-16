//! Shielded prover server binary.
//!
//! Run with:
//!   cargo run -p call-shielded --bin prover-server --features prover-server -- --bind 0.0.0.0:8080

#[cfg(feature = "prover-server")]
#[tokio::main]
async fn main() {
    use clap::Parser;
    use call_shielded::prover_server::{ProverMode, ProverState, run};
    use tracing_subscriber;

    #[derive(Parser, Debug)]
    #[command(name = "prover-server")]
    #[command(about = "HTTP prover server for shielded transactions")]
    struct Args {
        /// Bind address
        #[arg(short, long, default_value = "0.0.0.0:8080")]
        bind: String,

        /// Max QPS per API key
        #[arg(short, long, default_value_t = 10.0)]
        max_qps: f64,

        /// Proof cache TTL in seconds
        #[arg(long, default_value_t = 300)]
        cache_ttl: u64,

        /// API key for authentication (can be specified multiple times)
        #[arg(short, long)]
        api_key: Vec<String>,
    }

    let args = Args::parse();

    tracing_subscriber::fmt::init();

    let mode = if args.api_key.is_empty() {
        tracing::warn!("no api keys configured — running in dev mode");
        ProverMode::Dev
    } else {
        ProverMode::Production
    };

    let mut state = ProverState::new(mode, args.max_qps, args.cache_ttl);
    for key in args.api_key {
        state = state.with_api_key(key);
    }

    if let Err(e) = run(&args.bind, state).await {
        tracing::error!("server error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(not(feature = "prover-server"))]
fn main() {
    eprintln!("This binary requires the `prover-server` feature.");
    eprintln!("Run with: cargo run -p call-shielded --bin prover-server --features prover-server");
    std::process::exit(1);
}

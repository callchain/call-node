//! T14.1 — CLI argument definitions (per spec §21)

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Callchain node — CLI arguments (per spec §21.2)
#[derive(Parser, Debug)]
#[command(name = "calld", version, about = "Callchain Node")]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,

    // ── Mode ──────────────────────────────────────────────────────────
    /// Run as validator node (requires --validator-key)
    #[arg(long, default_value_t = false)]
    pub validator: bool,

    /// Solo mode: single-node validator that produces blocks without BFT consensus
    #[arg(long, default_value_t = false)]
    pub solo: bool,

    // ── Keys ──────────────────────────────────────────────────────────
    /// Validator consensus key (hex-encoded, devnet only)
    #[arg(long, hide = true)]
    pub validator_key: Option<String>,

    /// Path to encrypted validator keystore file (production)
    #[arg(long)]
    pub validator_keystore: Option<PathBuf>,

    /// Passphrase for validator keystore (or CALL_KEYSTORE_PASS env var)
    #[arg(long)]
    pub validator_keystore_pass: Option<String>,

    /// P2P identity key (hex-encoded, devnet only)
    #[arg(long, hide = true)]
    pub identity_key: Option<String>,

    /// Path to encrypted identity keystore file
    #[arg(long)]
    pub identity_keystore: Option<PathBuf>,

    /// Passphrase for identity keystore
    #[arg(long)]
    pub identity_keystore_pass: Option<String>,

    /// AWS KMS key ID or alias for validator signing (requires aws-kms feature)
    #[arg(long)]
    pub aws_kms_key_id: Option<String>,

    /// HashiCorp Vault address for validator signing (requires hashi-vault feature)
    #[arg(long)]
    pub vault_addr: Option<String>,

    /// HashiCorp Vault token for validator signing
    #[arg(long)]
    pub vault_token: Option<String>,

    /// HashiCorp Vault transit key name for validator signing
    #[arg(long)]
    pub vault_key_name: Option<String>,

    /// OS keyring service name for validator signing (requires keyring feature)
    #[arg(long)]
    pub keyring_service: Option<String>,

    /// OS keyring username/account for validator signing
    #[arg(long)]
    pub keyring_user: Option<String>,

    // ── Genesis ───────────────────────────────────────────────────────
    /// Path to genesis JSON file
    #[arg(long)]
    pub genesis_path: Option<PathBuf>,

    // ── Network / P2P ─────────────────────────────────────────────────
    /// P2P listen address
    #[arg(long, default_value = "0.0.0.0:51235")]
    pub p2p_listen_addr: SocketAddr,

    /// Bootstrap peers (format: "peer_id@address", comma-separated)
    #[arg(long)]
    pub p2p_bootstrap_peers: Option<String>,

    /// Maximum number of P2P peers (default: 50)
    #[arg(long)]
    pub p2p_max_peers: Option<u32>,

    // ── RPC ───────────────────────────────────────────────────────────
    /// HTTP RPC listen address
    #[arg(long, default_value = "127.0.0.1:8545")]
    pub http_addr: SocketAddr,

    /// WebSocket RPC listen address
    #[arg(long, default_value = "127.0.0.1:8546")]
    pub ws_addr: SocketAddr,

    /// Max concurrent RPC connections (default: 100)
    #[arg(long)]
    pub rpc_max_connections: Option<u32>,

    /// Path to TLS certificate (PEM) for HTTPS RPC
    #[arg(long)]
    pub tls_cert_path: Option<String>,

    /// Path to TLS private key (PEM, PKCS#8) for HTTPS RPC
    #[arg(long)]
    pub tls_key_path: Option<String>,

    /// Per-IP rate limit: max requests per window (default: disabled)
    #[arg(long)]
    pub rate_limit_rps: Option<u64>,

    /// Per-IP rate limit window in seconds (default: 60)
    #[arg(long, default_value_t = 60)]
    pub rate_limit_window_secs: u64,

    // ── Storage ───────────────────────────────────────────────────────
    /// Data directory for block/chain storage. When omitted, the value from
    /// the loaded TOML's `[storage] data_dir` is used, or otherwise
    /// `~/.callchain` (expanded via `dirs::home_dir`).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// DB cache size in MB (default: 1024)
    #[arg(long)]
    pub db_cache_size: Option<u64>,

    /// Archive mode: keep all historical state snapshots (disables pruning)
    #[arg(long, default_value_t = false)]
    pub archive: bool,

    // ── Metrics ───────────────────────────────────────────────────────
    /// Prometheus metrics listen address
    #[arg(long, default_value = "0.0.0.0:9090")]
    pub metrics_addr: SocketAddr,

    // ── Logging ───────────────────────────────────────────────────────
    /// Log level: trace, debug, info, warn, error
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Log format: text, json
    #[arg(long, default_value = "text")]
    pub log_format: String,

    // ── Governance ────────────────────────────────────────────────────
    /// Require secp256k1 signatures on governance RPC calls (disables unsigned devnet mode)
    #[arg(long, default_value_t = false)]
    pub require_governance_auth: bool,

    // ── Config file ───────────────────────────────────────────────────
    /// Path to TOML config file (CLI args override these values)
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the node (default behavior)
    Run {
        // Additional run-time args can be added here
    },

    /// Wallet subcommands
    #[command(subcommand)]
    Wallet(WalletCommand),
}

#[derive(Subcommand, Debug)]
pub enum WalletCommand {
    /// Generate a new keypair
    GenerateKeys,
    /// Derive address from public key
    Address {
        /// Hex-encoded public key (64 hex chars)
        #[arg(long)]
        pubkey: String,
    },
    /// Query account balance
    Balance {
        /// Account address
        #[arg(long)]
        address: String,
        /// Asset ID (default: 0 for CALL)
        #[arg(long, default_value_t = 0)]
        asset_id: u64,
        /// RPC URL
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },
    /// Send a payment
    Send {
        /// Hex-encoded secret key
        #[arg(long)]
        from_key: String,
        /// Recipient address
        #[arg(long)]
        to: String,
        /// Asset ID
        #[arg(long, default_value_t = 0)]
        asset_id: u64,
        /// Amount to send
        #[arg(long)]
        amount: u128,
        /// Nonce
        #[arg(long)]
        nonce: u64,
        /// RPC URL
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },
    /// Query node server info
    ServerInfo {
        /// RPC URL
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },
    /// Query mempool stats
    Mempool {
        /// RPC URL
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },
    /// Store a validator key in the OS keyring
    StoreKeyring {
        /// Hex-encoded 32-byte private key
        #[arg(long)]
        key: String,
        /// Keyring service name (default: call-node)
        #[arg(long, default_value = "call-node")]
        service: String,
        /// Keyring username / account name
        #[arg(long)]
        user: String,
    },
}

//! T14.1 — TOML configuration and CLI merge (per spec §21.2)

use crate::cli::CliArgs;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Full node configuration — loaded from TOML file, overridden by CLI.
#[derive(Debug, Clone, Deserialize)]
#[derive(Default)]
pub struct NodeConfig {
    #[serde(default)]
    pub mode: NodeMode,
    #[serde(default)]
    pub keys: KeysConfig,
    #[serde(default)]
    pub genesis: GenesisConfig,
    #[serde(default)]
    pub p2p: P2pConfig,
    #[serde(default)]
    pub rpc: RpcConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub governance: GovernanceConfig,
}


#[derive(Debug, Clone, Deserialize)]
#[derive(Default)]
pub struct GovernanceConfig {
    #[serde(default)]
    pub require_auth: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeMode {
    #[default]
    Full,
    Validator,
    Archive,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct KeysConfig {
    #[serde(default)]
    pub validator_key: Option<String>,
    #[serde(default)]
    pub validator_keystore: Option<PathBuf>,
    #[serde(default)]
    pub validator_keystore_pass: Option<String>,
    #[serde(default)]
    pub identity_key: Option<String>,
    #[serde(default)]
    pub identity_keystore: Option<PathBuf>,
    #[serde(default)]
    pub identity_keystore_pass: Option<String>,
    #[serde(default)]
    pub aws_kms_key_id: Option<String>,
    #[serde(default)]
    pub vault_addr: Option<String>,
    #[serde(default)]
    pub vault_token: Option<String>,
    #[serde(default)]
    pub vault_key_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[derive(Default)]
pub struct GenesisConfig {
    #[serde(default = "GenesisConfig::default_path")]
    pub path: Option<PathBuf>,
}


impl GenesisConfig {
    fn default_path() -> Option<PathBuf> { None }
}

#[derive(Debug, Clone, Deserialize)]
pub struct P2pConfig {
    #[serde(default = "P2pConfig::default_listen_addr")]
    pub listen_addr: SocketAddr,
    #[serde(default)]
    pub bootstrap_peers: Option<String>,
    #[serde(default = "P2pConfig::default_max_peers")]
    pub max_peers: u32,
}

impl Default for P2pConfig {
    fn default() -> Self {
        Self {
            listen_addr: Self::default_listen_addr(),
            bootstrap_peers: None,
            max_peers: Self::default_max_peers(),
        }
    }
}

impl P2pConfig {
    fn default_listen_addr() -> SocketAddr {
        "0.0.0.0:51235".parse().unwrap()
    }
    fn default_max_peers() -> u32 { 50 }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    #[serde(default = "RpcConfig::default_http_addr")]
    pub http_addr: SocketAddr,
    #[serde(default = "RpcConfig::default_ws_addr")]
    pub ws_addr: SocketAddr,
    #[serde(default = "RpcConfig::default_max_connections")]
    pub max_connections: u32,
    /// Path to TLS certificate (PEM). Both cert and key must be set to enable TLS.
    #[serde(default)]
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key (PEM, PKCS#8).
    #[serde(default)]
    pub tls_key_path: Option<String>,
    /// Max requests per IP per window. None = no rate limiting.
    #[serde(default)]
    pub rate_limit_rps: Option<u64>,
    /// Rate-limit window in seconds.
    #[serde(default = "RpcConfig::default_rate_limit_window_secs")]
    pub rate_limit_window_secs: u64,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            http_addr: Self::default_http_addr(),
            ws_addr: Self::default_ws_addr(),
            max_connections: Self::default_max_connections(),
            tls_cert_path: None,
            tls_key_path: None,
            rate_limit_rps: None,
            rate_limit_window_secs: Self::default_rate_limit_window_secs(),
        }
    }
}

impl RpcConfig {
    fn default_http_addr() -> SocketAddr { "127.0.0.1:8545".parse().unwrap() }
    fn default_ws_addr() -> SocketAddr { "127.0.0.1:8546".parse().unwrap() }
    fn default_max_connections() -> u32 { 100 }
    fn default_rate_limit_window_secs() -> u64 { 60 }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "StorageConfig::default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "StorageConfig::default_cache_size")]
    pub db_cache_size: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: Self::default_data_dir(),
            db_cache_size: Self::default_cache_size(),
        }
    }
}

impl StorageConfig {
    fn default_data_dir() -> PathBuf {
        dirs::home_dir()
            .map(|d| d.join(".callchain"))
            .unwrap_or_else(|| PathBuf::from(".callchain"))
    }
    fn default_cache_size() -> u64 { 1024 }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    #[serde(default = "MetricsConfig::default_addr")]
    pub addr: SocketAddr,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            addr: Self::default_addr(),
        }
    }
}

impl MetricsConfig {
    fn default_addr() -> SocketAddr { "0.0.0.0:9090".parse().unwrap() }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "LoggingConfig::default_level")]
    pub level: String,
    #[serde(default = "LoggingConfig::default_format")]
    pub format: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Self::default_level(),
            format: Self::default_format(),
        }
    }
}

impl LoggingConfig {
    fn default_level() -> String { "info".into() }
    fn default_format() -> String { "text".into() }
}

// ── Merge: CLI overrides TOML ────────────────────────────────────────

impl NodeConfig {
    /// Load from TOML file
    pub fn from_file(path: &PathBuf) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read config file: {e}"))?;
        toml::from_str(&content)
            .map_err(|e| format!("failed to parse config file: {e}"))
    }

    /// Apply CLI arguments on top of this config (CLI takes priority)
    pub fn merge_from_cli(mut self, args: &CliArgs) -> Self {
        // Mode
        if args.validator {
            self.mode = NodeMode::Validator;
        }

        // Keys
        if args.validator_key.is_some() {
            self.keys.validator_key.clone_from(&args.validator_key);
        }
        if args.validator_keystore.is_some() {
            self.keys.validator_keystore.clone_from(&args.validator_keystore);
        }
        if args.validator_keystore_pass.is_some() {
            self.keys.validator_keystore_pass.clone_from(&args.validator_keystore_pass);
        }
        if args.identity_key.is_some() {
            self.keys.identity_key.clone_from(&args.identity_key);
        }
        if args.identity_keystore.is_some() {
            self.keys.identity_keystore.clone_from(&args.identity_keystore);
        }
        if args.identity_keystore_pass.is_some() {
            self.keys.identity_keystore_pass.clone_from(&args.identity_keystore_pass);
        }
        if args.aws_kms_key_id.is_some() {
            self.keys.aws_kms_key_id.clone_from(&args.aws_kms_key_id);
        }
        if args.vault_addr.is_some() {
            self.keys.vault_addr.clone_from(&args.vault_addr);
        }
        if args.vault_token.is_some() {
            self.keys.vault_token.clone_from(&args.vault_token);
        }
        if args.vault_key_name.is_some() {
            self.keys.vault_key_name.clone_from(&args.vault_key_name);
        }

        // Genesis
        if args.genesis_path.is_some() {
            self.genesis.path.clone_from(&args.genesis_path);
        }

        // P2P
        if args.p2p_listen_addr != "0.0.0.0:51235".parse().unwrap() {
            self.p2p.listen_addr = args.p2p_listen_addr;
        }
        if args.p2p_bootstrap_peers.is_some() {
            self.p2p.bootstrap_peers.clone_from(&args.p2p_bootstrap_peers);
        }
        if let Some(max_peers) = args.p2p_max_peers {
            self.p2p.max_peers = max_peers;
        }

        // RPC
        if args.http_addr != "127.0.0.1:8545".parse().unwrap() {
            self.rpc.http_addr = args.http_addr;
        }
        if args.ws_addr != "127.0.0.1:8546".parse().unwrap() {
            self.rpc.ws_addr = args.ws_addr;
        }
        if let Some(max_conn) = args.rpc_max_connections {
            self.rpc.max_connections = max_conn;
        }
        if args.tls_cert_path.is_some() {
            self.rpc.tls_cert_path.clone_from(&args.tls_cert_path);
        }
        if args.tls_key_path.is_some() {
            self.rpc.tls_key_path.clone_from(&args.tls_key_path);
        }
        if args.rate_limit_rps.is_some() {
            self.rpc.rate_limit_rps = args.rate_limit_rps;
        }
        if args.rate_limit_window_secs != 60 {
            self.rpc.rate_limit_window_secs = args.rate_limit_window_secs;
        }

        // Storage
        if args.data_dir != Self::default().storage.data_dir {
            self.storage.data_dir.clone_from(&args.data_dir);
        }
        if let Some(cache) = args.db_cache_size {
            self.storage.db_cache_size = cache;
        }

        // Metrics
        if args.metrics_addr != "0.0.0.0:9090".parse().unwrap() {
            self.metrics.addr = args.metrics_addr;
        }

        // Logging
        if args.log_level != "info" {
            self.logging.level.clone_from(&args.log_level);
        }
        if args.log_format != "text" {
            self.logging.format.clone_from(&args.log_format);
        }

        // Governance
        if args.require_governance_auth {
            self.governance.require_auth = true;
        }

        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == NodeMode::Validator {
            let key_sources = [
                ("--validator-key", self.keys.validator_key.is_some()),
                ("--validator-keystore", self.keys.validator_keystore.is_some()),
                ("--aws-kms-key-id", self.keys.aws_kms_key_id.is_some()),
                ("--vault-addr", self.keys.vault_addr.is_some()),
            ];
            let active_sources: Vec<_> = key_sources.iter().filter(|(_, active)| *active).collect();

            if active_sources.is_empty() {
                return Err("validator mode requires one key source: --validator-key, --validator-keystore, --aws-kms-key-id, or --vault-addr".into());
            }
            if active_sources.len() > 1 {
                return Err(format!(
                    "provide exactly one key source, got: {}",
                    active_sources.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
                ));
            }
            if let Some(ref key) = self.keys.validator_key {
                if key.len() != 128 {
                    return Err("validator_key must be 64 hex bytes (128 hex chars)".into());
                }
            }
            if let Some(ref path) = self.keys.validator_keystore {
                if !path.exists() {
                    return Err(format!("validator keystore file not found: {}", path.display()));
                }
            }
            if self.keys.vault_addr.is_some() {
                if self.keys.vault_token.is_none() {
                    return Err("--vault-token required when using --vault-addr".into());
                }
                if self.keys.vault_key_name.is_none() {
                    return Err("--vault-key-name required when using --vault-addr".into());
                }
            }
        }
        if let Some(ref key) = self.keys.identity_key {
            let hex_clean = key.trim_start_matches("0x");
            if hex_clean.len() != 64 && hex_clean.len() != 128 {
                return Err("identity_key must be 32 hex bytes (64 hex chars) or 64 hex bytes (128 hex chars)".into());
            }
        }
        if self.keys.identity_key.is_some() && self.keys.identity_keystore.is_some() {
            return Err("provide either --identity-key or --identity-keystore, not both".into());
        }
        Ok(())
    }
}

/// Parse bootstrap peers string into list of (peer_id, address)
pub fn parse_bootstrap_peers(peers: &Option<String>) -> Vec<(String, SocketAddr)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn valid_key() -> String {
        "a".repeat(128)
    }

    #[test]
    fn test_cli_parse_all_args() {
        use std::ffi::OsString;

        let args = CliArgs::parse_from([
            "calld",
            "--validator",
            "--validator-key", &valid_key(),
            "--identity-key", &valid_key(),
            "--genesis-path", "/tmp/genesis.json",
            "--p2p-listen-addr", "0.0.0.0:6000",
            "--p2p-bootstrap-peers", "peer1@127.0.0.1:51235",
            "--p2p-max-peers", "100",
            "--http-addr", "127.0.0.1:9545",
            "--ws-addr", "127.0.0.1:9546",
            "--rpc-max-connections", "200",
            "--data-dir", "/tmp/callchain",
            "--db-cache-size", "2048",
            "--metrics-addr", "0.0.0.0:9091",
            "--log-level", "debug",
            "--log-format", "json",
            "--config", "/tmp/config.toml",
        ]);

        assert!(args.validator);
        assert_eq!(args.validator_key, Some(valid_key()));
        assert_eq!(args.identity_key, Some(valid_key()));
        assert_eq!(args.genesis_path, Some(PathBuf::from("/tmp/genesis.json")));
        assert_eq!(args.p2p_listen_addr, "0.0.0.0:6000".parse().unwrap());
        assert_eq!(args.p2p_bootstrap_peers, Some("peer1@127.0.0.1:51235".into()));
        assert_eq!(args.p2p_max_peers, Some(100));
        assert_eq!(args.http_addr, "127.0.0.1:9545".parse().unwrap());
        assert_eq!(args.ws_addr, "127.0.0.1:9546".parse().unwrap());
        assert_eq!(args.rpc_max_connections, Some(200));
        assert_eq!(args.data_dir, PathBuf::from("/tmp/callchain"));
        assert_eq!(args.db_cache_size, Some(2048));
        assert_eq!(args.metrics_addr, "0.0.0.0:9091".parse().unwrap());
        assert_eq!(args.log_level, "debug");
        assert_eq!(args.log_format, "json");
        assert_eq!(args.config, Some(PathBuf::from("/tmp/config.toml")));
    }

    #[test]
    fn test_toml_config_parse() {
        let toml_content = r#"
mode = "validator"

[keys]
validator_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
identity_key = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

[genesis]
path = "/tmp/genesis.json"

[p2p]
listen_addr = "0.0.0.0:6000"
bootstrap_peers = "peer1@127.0.0.1:51235"
max_peers = 100

[rpc]
http_addr = "127.0.0.1:9545"
ws_addr = "127.0.0.1:9546"
max_connections = 200

[storage]
data_dir = "/tmp/callchain"
db_cache_size = 2048

[metrics]
addr = "0.0.0.0:9091"

[logging]
level = "debug"
format = "json"
"#;

        let path = std::env::temp_dir().join("call_test_config.toml");
        std::fs::write(&path, toml_content).unwrap();

        let config = NodeConfig::from_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(config.mode, NodeMode::Validator);
        assert!(config.keys.validator_key.is_some());
        assert!(config.keys.identity_key.is_some());
        assert_eq!(config.genesis.path, Some(PathBuf::from("/tmp/genesis.json")));
        assert_eq!(config.p2p.listen_addr, "0.0.0.0:6000".parse().unwrap());
        assert_eq!(config.p2p.bootstrap_peers, Some("peer1@127.0.0.1:51235".into()));
        assert_eq!(config.p2p.max_peers, 100);
        assert_eq!(config.rpc.http_addr, "127.0.0.1:9545".parse().unwrap());
        assert_eq!(config.rpc.ws_addr, "127.0.0.1:9546".parse().unwrap());
        assert_eq!(config.rpc.max_connections, 200);
        assert_eq!(config.storage.data_dir, PathBuf::from("/tmp/callchain"));
        assert_eq!(config.storage.db_cache_size, 2048);
        assert_eq!(config.metrics.addr, "0.0.0.0:9091".parse().unwrap());
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.logging.format, "json");
    }

    #[test]
    fn test_cli_overrides_toml() {
        let toml_content = r#"
[p2p]
listen_addr = "0.0.0.0:6000"

[rpc]
http_addr = "127.0.0.1:9545"

[logging]
level = "warn"
"#;

        let path = std::env::temp_dir().join("call_test_override.toml");
        std::fs::write(&path, toml_content).unwrap();

        let base = NodeConfig::from_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        // CLI with different values
        let args = CliArgs::parse_from([
            "calld",
            "--p2p-listen-addr", "0.0.0.0:7000",
            "--http-addr", "127.0.0.1:18545",
            "--log-level", "trace",
        ]);

        let config = base.merge_from_cli(&args);

        // CLI values should override TOML
        assert_eq!(config.p2p.listen_addr, "0.0.0.0:7000".parse().unwrap());
        assert_eq!(config.rpc.http_addr, "127.0.0.1:18545".parse().unwrap());
        assert_eq!(config.logging.level, "trace");

        // TOML values not overridden by CLI should remain
        assert_eq!(config.rpc.ws_addr, "127.0.0.1:8546".parse().unwrap()); // default, not in TOML or CLI
        assert_eq!(config.logging.format, "text"); // default, not overridden
    }

    #[test]
    fn test_boot_sequence_fresh_db() {
        let data_dir = std::env::temp_dir().join("call_fresh_db_test");
        let _ = std::fs::remove_dir_all(&data_dir);

        let config = NodeConfig::default().merge_from_cli(&CliArgs::parse_from([
            "calld",
            "--data-dir", data_dir.to_str().unwrap(),
            "--log-level", "warn",
        ]));

        // Fresh DB should not have blocks directory
        assert!(!data_dir.join("blocks").exists());

        // After boot, blocks dir should be created (CallNode::new does this)
        let node = crate::CallNode::new(config.storage.data_dir.clone());
        assert!(node.is_ok());
        // Clean up
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn test_boot_sequence_existing_db_recovery() {
        let data_dir = std::env::temp_dir().join("call_existing_db_test");
        let _ = std::fs::remove_dir_all(&data_dir);

        // Simulate existing data
        std::fs::create_dir_all(data_dir.join("blocks")).unwrap();
        std::fs::write(data_dir.join("blocks").join("000000000000.json"), b"{}").unwrap();

        let config = NodeConfig::default().merge_from_cli(&CliArgs::parse_from([
            "calld",
            "--data-dir", data_dir.to_str().unwrap(),
            "--log-level", "warn",
        ]));

        // Existing data should be detected
        assert!(config.storage.data_dir.join("blocks").exists());

        // Node should boot successfully (recovery path)
        let node = crate::CallNode::new(config.storage.data_dir.clone());
        assert!(node.is_ok());

        // Clean up
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn test_validator_mode_requires_keys() {
        // Default mode (Full) should validate OK
        let config = NodeConfig::default();
        assert!(config.validate().is_ok());

        // Validator without key should fail
        let mut config = NodeConfig::default();
        config.mode = NodeMode::Validator;
        assert!(config.validate().is_err());
        let err = config.validate().unwrap_err();
        assert!(err.contains("validator-key"));

        // Validator with key should pass
        config.keys.validator_key = Some(valid_key());
        assert!(config.validate().is_ok());

        // Invalid key length should fail
        config.keys.validator_key = Some("short".into());
        assert!(config.validate().is_err());
        let err = config.validate().unwrap_err();
        assert!(err.contains("128 hex chars"));

        // Archive mode without key should pass
        let mut config = NodeConfig::default();
        config.mode = NodeMode::Archive;
        assert!(config.validate().is_ok());
    }
}

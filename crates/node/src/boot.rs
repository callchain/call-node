//! T14.1 — Boot sequence (per spec §21.3)
//!
//! parse config → init logging → open DB → load genesis → init P2P →
//! connect seeds → init consensus → start RPC → sync/participate

use crate::config::{NodeConfig, NodeMode, parse_bootstrap_peers};
use crate::CallNode;
use call_network::{CommonwareConfig, NetworkLimits, load_or_generate_identity_key};
use call_primitives::Address;
use call_rpc::RpcConfig;
use call_crypto::{LocalSigner, SignerRef, load_key as load_keystore_key, bls_generate, bls_public_key_bytes};
use commonware_cryptography::ed25519;
use commonware_codec::extensions::DecodeExt;
use rand::rngs::OsRng;
use serde::Deserialize;
use std::fs;
use std::sync::Arc;
use tracing::info;

/// Result type for boot sequence
pub type BootResult = Result<CallNode, String>;

/// Genesis file format: balances, assets, validators, timestamp.
#[derive(Debug, Clone, Deserialize)]
pub struct Genesis {
    #[serde(default)]
    pub balances: Vec<GenesisBalance>,
    #[serde(default)]
    pub validators: Vec<GenesisValidator>,
    #[serde(default = "Genesis::default_timestamp")]
    pub timestamp: u64,
    /// Asset IDs to track for oracle price submissions
    #[serde(default)]
    pub oracle_assets: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisBalance {
    pub address: String,
    pub asset_id: u64,
    pub amount: u128,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisValidator {
    pub address: String,
    pub pubkey: String,
    pub stake: u128,
}

impl Genesis {
    fn default_timestamp() -> u64 {
        1_000_000 // default genesis timestamp
    }

    /// Load and parse a genesis file from disk
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read genesis file: {e}"))?;
        let genesis: Genesis = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse genesis JSON: {e}"))?;
        Ok(genesis)
    }

    /// Apply genesis state to the node: balances and validators
    pub fn apply(&self, node: &mut CallNode) -> Result<(), String> {
        // Apply genesis balances
        let mut balance_state = node.state.balance_state.write().map_err(|_| "lock poisoned")?;
        for entry in &self.balances {
            let addr = parse_address(&entry.address)?;
            balance_state
                .balances
                .set_balance(entry.asset_id, addr, entry.amount)
                .map_err(|e| format!("failed to set genesis balance: {e}"))?;
        }

        // Stake genesis validators
        let mut consensus = node.consensus.write().map_err(|_| "lock poisoned")?;
        for val in &self.validators {
            let addr = parse_address(&val.address)?;
            let pubkey = parse_pubkey(&val.pubkey)?;
            consensus
                .stake_validator(addr, pubkey, val.stake)
                .map_err(|e| format!("failed to stake validator: {e}"))?;
        }
        consensus.refresh_proposer_subset();
        drop(consensus);

        // Register genesis validators into governance for voting
        {
            let mut gov = node.state.governance.write().map_err(|_| "lock poisoned")?;
            for (i, val) in self.validators.iter().enumerate() {
                let addr = parse_address(&val.address)?;
                gov.register_validator(i as u32, addr);
            }
        }

        Ok(())
    }
}

fn parse_address(s: &str) -> Result<Address, String> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| format!("invalid address hex: {e}"))?;
    if bytes.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", bytes.len()));
    }
    Ok(Address::from_slice(&bytes))
}

fn parse_pubkey(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| format!("invalid pubkey hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("pubkey must be 32 bytes, got {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Load or derive an ed25519 private key for BFT consensus.
///
/// Priority:
/// 1. `identity_key` config (first 32 bytes of the 64-byte hex string)
/// 2. Generate a random key (warns — set `identity_key` for production)
fn load_ed25519_key(keys: &crate::config::KeysConfig) -> Result<ed25519::PrivateKey, String> {
    if let Some(ref hex_key) = keys.identity_key {
        let hex_clean = hex_key.trim_start_matches("0x");
        // Accept either 64 hex chars (32 bytes) or 128 hex chars (64 bytes, use first 32)
        let seed_hex = if hex_clean.len() == 128 {
            &hex_clean[..64]
        } else if hex_clean.len() == 64 {
            hex_clean
        } else {
            return Err(format!(
                "identity_key must be 64 or 128 hex chars, got {}",
                hex_clean.len()
            ));
        };
        let bytes = hex::decode(seed_hex).map_err(|e| format!("invalid identity_key hex: {e}"))?;
        ed25519::PrivateKey::decode(&bytes[..]).map_err(|e| format!("invalid ed25519 key: {e}"))
    } else {
        let mut seed = [0u8; 32];
        rand::RngCore::fill_bytes(&mut OsRng, &mut seed);
        let key = ed25519::PrivateKey::decode(&seed[..])
            .map_err(|e| format!("failed to decode random ed25519 key: {e}"))?;
        info!("generated random ed25519 consensus key — set identity_key in config for deterministic identity across restarts");
        Ok(key)
    }
}

/// Load validator signer from configured key source
async fn load_validator_signer(keys: &crate::config::KeysConfig) -> Result<SignerRef, String> {
    if let Some(ref path) = keys.validator_keystore {
        let pass = keys.validator_keystore_pass
            .clone()
            .or_else(|| std::env::var("CALL_KEYSTORE_PASS").ok())
            .ok_or("validator keystore passphrase required (via --validator-keystore-pass or CALL_KEYSTORE_PASS env var)")?;
        let raw_key = load_keystore_key(path, &pass)
            .map_err(|e| format!("failed to load validator keystore: {e}"))?;
        let signer = LocalSigner::from_raw_key(raw_key)
            .map_err(|e| format!("invalid validator key: {e}"))?;
        Ok(Arc::new(signer))
    } else if let Some(ref hex_key) = keys.validator_key {
        let signer = LocalSigner::from_hex(hex_key)
            .map_err(|e| format!("invalid validator key: {e}"))?;
        info!("WARNING: using plaintext validator key — use --validator-keystore for production");
        Ok(Arc::new(signer))
    } else if let Some(ref key_id) = keys.aws_kms_key_id {
        #[cfg(feature = "aws-kms")]
        {
            let signer = call_crypto::AwsKmsSigner::new(key_id.clone()).await
                .map_err(|e| format!("failed to create AWS KMS signer: {e}"))?;
            Ok(Arc::new(signer))
        }
        #[cfg(not(feature = "aws-kms"))]
        {
            let _ = key_id;
            Err("AWS KMS support not compiled in (enable aws-kms feature)".into())
        }
    } else if let Some(ref vault_addr) = keys.vault_addr {
        #[cfg(feature = "hashi-vault")]
        {
            let token = keys.vault_token.clone()
                .ok_or("--vault-token required")?;
            let key_name = keys.vault_key_name.clone()
                .ok_or("--vault-key-name required")?;
            let signer = call_crypto::HashiVaultSigner::new(vault_addr.clone(), token, key_name).await
                .map_err(|e| format!("failed to create Vault signer: {e}"))?;
            Ok(Arc::new(signer))
        }
        #[cfg(not(feature = "hashi-vault"))]
        {
            let _ = vault_addr;
            Err("HashiVault support not compiled in (enable hashi-vault feature)".into())
        }
    } else {
        Err("validator key required (use --validator-keystore, --validator-key, --aws-kms-key-id, or --vault-addr)".into())
    }
}

/// Execute the full boot sequence per §21.3.
pub async fn boot_node(config: &NodeConfig) -> BootResult {
    // Step 1: Open DB (resume from existing data if present)
    info!(data_dir = ?config.storage.data_dir, "step 1: opening database");
    let existing_data = config.storage.data_dir.join("blocks").exists();
    if existing_data {
        info!("existing data found — resuming from last saved state");
    } else {
        info!("fresh database — will initialize from genesis");
    }

    // Step 2: Create node (opens DB, initializes state)
    info!("step 2: initializing node");
    let mut node = CallNode::new(config.storage.data_dir.clone())?;

    // Wire governance auth config
    node.state.set_governance_auth(config.governance.require_auth);

    // Step 2b: Load validator signing key (if validator mode)
    if config.mode == NodeMode::Validator {
        let signer = load_validator_signer(&config.keys).await?;
        info!(
            address = ?signer.address(),
            kind = ?signer.kind(),
            "validator signing key loaded"
        );
        *node.state.signer.write().map_err(|_| "lock poisoned")? = Some(signer);

        // Generate BLS12-381 keypair for aggregated vote signing
        let (bls_secret, bls_pubkey) = bls_generate()
            .map_err(|e| format!("failed to generate BLS keypair: {e}"))?;
        *node.state.bls_secret_key.write().map_err(|_| "lock poisoned")? = Some(bls_secret);

        // Register BLS pubkey with the validator state if this validator is known
        {
            let signer_guard = node.state.signer.read().map_err(|_| "lock poisoned")?;
            if let Some(ref s) = *signer_guard {
                let validator_addr = s.address();
                let mut vs = node.state.validator_state.write().map_err(|_| "lock poisoned")?;
                for (id, stake) in vs.get_all_validators().clone().iter() {
                    if stake.address == validator_addr {
                        let _ = vs.set_validator_bls_pubkey(*id, bls_public_key_bytes(&bls_pubkey));
                        info!(validator_id = id, "registered BLS pubkey for validator");
                        break;
                    }
                }
            }
        }
    }

    // Step 3: Load genesis if path provided
    if let Some(ref genesis_path) = config.genesis.path {
        info!(path = ?genesis_path, "step 3: loading genesis");
        let genesis = Genesis::load(genesis_path)?;
        info!(
            balances = genesis.balances.len(),
            validators = genesis.validators.len(),
            "genesis loaded"
        );
        genesis.apply(&mut node)?;

        // Register genesis validators into the oracle for price submissions
        let mut oracle = node.state.oracle.write().map_err(|_| "lock poisoned")?;
        for (i, val) in genesis.validators.iter().enumerate() {
            let addr = parse_address(&val.address)?;
            let pubkey = parse_pubkey(&val.pubkey)?;
            oracle.register_validator(i as u32, addr, pubkey);
        }
        if !genesis.oracle_assets.is_empty() {
            oracle.set_tracked_assets(genesis.oracle_assets.clone());
        }
        drop(oracle);
    }

    // Step 4: Init P2P and connect seeds
    info!(listen = %config.p2p.listen_addr, "step 4: initializing P2P");
    let identity_key = load_or_generate_identity_key(&config.storage.data_dir, config.keys.identity_key.as_deref())?;
    let bootstrap = parse_bootstrap_peers(&config.p2p.bootstrap_peers);
    let p2p_config = CommonwareConfig {
        listen_addr: config.p2p.listen_addr,
        bootstrap_peers: bootstrap,
        max_message_size: 10 * 1024 * 1024,
        allow_private_ips: config.mode != NodeMode::Validator,
        namespace: b"callchain".to_vec(),
        min_healthy_peers: if config.mode == NodeMode::Validator { 1 } else { 0 },
        limits: NetworkLimits::default(),
    };
    node.start_network(p2p_config, identity_key).await?;

    // Step 5: Init consensus (validator or full node)
    match config.mode {
        NodeMode::Validator => {
            info!("step 5: starting in validator mode");
        }
        NodeMode::Full => {
            info!("step 5: starting in full node mode");
        }
        NodeMode::Archive => {
            info!("step 5: starting in archive node mode");
        }
    }

    // Step 6: Start RPC
    info!(http = %config.rpc.http_addr, ws = %config.rpc.ws_addr, "step 6: starting RPC");
    let rpc_config = RpcConfig {
        http_addr: config.rpc.http_addr,
        ws_addr: config.rpc.ws_addr,
        max_connections: config.rpc.max_connections,
        tls_cert_path: config.rpc.tls_cert_path.clone(),
        tls_key_path: config.rpc.tls_key_path.clone(),
        rate_limit_rps: config.rpc.rate_limit_rps,
        rate_limit_window_secs: config.rpc.rate_limit_window_secs,
    };
    node.start_rpc(rpc_config.clone()).await?;
    node.start_ws_rpc(rpc_config).await?;

    // Step 7: Start consensus block production
    match config.mode {
        NodeMode::Validator => {
            info!("step 7: starting BFT consensus engine");
            let ed25519_key = load_ed25519_key(&config.keys)?;
            let consensus_p2p_port = config.p2p.listen_addr.port().saturating_add(1);
            let _handle = node.start_bft_engine(ed25519_key, consensus_p2p_port);
        }
        NodeMode::Full | NodeMode::Archive => {
            info!("step 7: starting full-node consensus loop");
            let _handle = node.start_consensus_loop();
        }
    }

    // Step 8: Start compliance data sync (if URL configured)
    let compliance_url = std::env::var("CALL_COMPLIANCE_DATA_URL").ok();
    if compliance_url.is_some() {
        info!("step 8: starting compliance data sync");
    }
    let _compliance_handle = node.start_compliance_sync(compliance_url, 300); // 5 min interval

    info!(
        mode = ?config.mode,
        http = %config.rpc.http_addr,
        ws = %config.rpc.ws_addr,
        p2p = %config.p2p.listen_addr,
        metrics = %config.metrics.addr,
        "node booted successfully"
    );

    Ok(node)
}

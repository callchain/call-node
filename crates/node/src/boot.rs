//! T14.1 — Boot sequence (per spec §21.3)
//!
//! parse config → init logging → open DB → load genesis → init P2P →
//! connect seeds → init consensus → start RPC → sync/participate

use crate::config::{NodeConfig, NodeMode, parse_bootstrap_peers};
use crate::CallNode;
use call_consensus::SimplexConsensus;
use call_network::{CommonwareConfig, NetworkLimits, load_or_generate_identity_key};
use call_primitives::Address;
use call_rpc::RpcConfig;
use call_crypto::{LocalSigner, SignerRef, load_key as load_keystore_key, bls_generate, bls_public_key_bytes};
use commonware_cryptography::ed25519;
use commonware_codec::extensions::DecodeExt;
use rand::rngs::OsRng;
use std::sync::Arc;
use tracing::info;

/// Result type for boot sequence
pub type BootResult = Result<CallNode, String>;

fn parse_address(s: &str) -> Result<Address, String> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| format!("invalid address hex: {e}"))?;
    if bytes.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", bytes.len()));
    }
    Ok(Address::from_slice(&bytes))
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

    // Pre-load genesis to extract chain_id before node creation
    let preloaded_genesis = if let Some(ref genesis_path) = config.genesis.path {
        info!(path = ?genesis_path, "pre-loading genesis for chain_id");
        Some(call_chainspec::Genesis::load_from_file(genesis_path)
            .map_err(|e| format!("failed to load genesis: {e}"))?)
    } else {
        None
    };
    let genesis_chain_id = preloaded_genesis.as_ref().map(|g| g.chain_id);

    // Step 2: Create node (opens DB, initializes state)
    info!("step 2: initializing node");
    let mut node = CallNode::new_with_chain_id(config.storage.data_dir.clone(), genesis_chain_id)?;

    // Wire governance auth config
    node.state.set_governance_auth(config.governance.require_auth);

    // Step 2b: Initialize shielded ZK prover (production keys if compiled with production-keys feature)
    #[cfg(feature = "production-keys")]
    {
        info!("initializing shielded prover");
        let _ = call_shielded::RealProver::global();
        info!("shielded prover ready");
    }

    // Step 2c: Load validator signing key (if validator mode)
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

        // Register BLS pubkey with the validator state in EVM storage if this validator is known
        {
            let signer_guard = node.state.signer.read().map_err(|_| "lock poisoned")?;
            if let Some(ref s) = *signer_guard {
                let validator_addr = s.address();
                let mut evm_state = node.state.evm_state.write().map_err(|_| "lock poisoned")?;
                let validator_id = call_consensus::exec::evm_instructions::read_validator_id_by_addr(
                    &evm_state, validator_addr);
                if validator_id != 0 {
                    call_consensus::exec::evm_instructions::set_validator_bls_pubkey(
                        &mut evm_state, validator_addr, bls_public_key_bytes(&bls_pubkey));
                    info!(validator_id = validator_id, "registered BLS pubkey for validator in EVM storage");
                }
            }
        }
    }

    // Step 3: Apply genesis if this is a fresh start
    if node.fresh_start {
        if let Some(genesis) = preloaded_genesis {
            info!(path = ?config.genesis.path, "step 3: executing genesis");
            info!(
                assets = genesis.initial_assets.len(),
                validators = genesis.validators.len(),
                "genesis loaded"
            );

            let executor = call_chainspec::GenesisExecutor::new(genesis.clone());
            let genesis_state = executor.execute()
                .map_err(|e| format!("failed to execute genesis: {e}"))?;

            // Build consensus before moving genesis EVM state into RpcState
            let new_consensus = SimplexConsensus::new(genesis.consensus_params.clone(), &genesis_state.evm_state);
            // Inject genesis state into RpcState
            *node.state.evm_state.write().map_err(|_| "lock poisoned")? = genesis_state.evm_state;

            *node.consensus.write().map_err(|_| "lock poisoned")? = new_consensus;

            // Sync consensus params into RpcState
            {
                let consensus = node.consensus.read().map_err(|_| "lock poisoned")?;
                *node.state.consensus_params.write().map_err(|_| "lock poisoned")? = *consensus.params();
            }

            // Register genesis validators into governance for voting
            {
                let mut gov = node.governance.write().map_err(|_| "lock poisoned")?;
                for (i, val) in genesis.validators.iter().enumerate() {
                    let addr = parse_address(&val.address)?;
                    gov.register_validator(i as u32, addr);
                }
            }

            info!("genesis applied successfully");
        } else {
            info!("step 3: no genesis path configured, starting with empty state");
        }
    } else {
        info!("step 3: existing data found, skipping genesis");
    }

    // Step 4: Init P2P and connect seeds
    info!(listen = %config.p2p.listen_addr, "step 4: initializing P2P");
    let identity_key = load_or_generate_identity_key(&config.storage.data_dir, config.keys.identity_key.as_deref())?;
    let bootstrap = parse_bootstrap_peers(&config.p2p.bootstrap_peers);
    // Default: validators reject private IPs (production semantics); full/archive nodes accept them.
    // Operators can override via `[p2p] allow_private_ips = true` (required for local devnets where
    // every node lives on an RFC1918 subnet, e.g. the docker-compose devnet on 172.28.0.0/16).
    let allow_private_ips = config
        .p2p
        .allow_private_ips
        .unwrap_or(config.mode != NodeMode::Validator);
    let p2p_config = CommonwareConfig {
        listen_addr: config.p2p.listen_addr,
        bootstrap_peers: bootstrap.clone(),
        max_message_size: 10 * 1024 * 1024,
        allow_private_ips,
        namespace: b"callchain".to_vec(),
        min_healthy_peers: if config.mode == NodeMode::Validator { 1 } else { 0 },
        limits: NetworkLimits::default(),
        ..Default::default()
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
        cors_allowed_origins: config.rpc.cors_allowed_origins.clone(),
    };
    node.start_rpc(rpc_config.clone()).await?;
    node.start_ws_rpc(rpc_config).await?;

    // Step 7: Start consensus block production
    match config.mode {
        NodeMode::Validator => {
            if config.solo {
                info!("step 7: solo validator mode — starting local block production (no BFT)");
                let _handle = node.start_consensus_loop();
            } else {
                info!("step 7: starting BFT consensus engine");
                let ed25519_key = load_ed25519_key(&config.keys)?;
                let consensus_p2p_port = config.p2p.listen_addr.port().saturating_add(1);

                // Build the set of validator pubkeys from the genesis we just
                // applied. Only entries whose pubkey is in this set may appear in
                // the BFT P2P bootstrap list — the gossip bootstrap may legally
                // contain non-validator peers (full / archive nodes added so the
                // validator's authenticated p2p layer accepts inbound connections
                // from them), and including those in the BFT bootstrap would have
                // the consensus network try to dial nodes that aren't running a
                // BFT engine at all.
                let validator_pubkeys: std::collections::HashSet<Vec<u8>> = {
                    let evm_state = node.state.evm_state.read().map_err(|_| "evm_state poisoned")?;
                    let count = call_consensus::exec::evm_instructions::read_validator_count(&evm_state);
                    let mut set = std::collections::HashSet::new();
                    for id in 1..=count {
                        let addr = call_consensus::exec::evm_instructions::read_validator_addr(&evm_state, id);
                        if addr != call_primitives::Address::ZERO {
                            let pk = call_consensus::exec::evm_instructions::read_validator_pubkey(
                                &evm_state, addr);
                            set.insert(pk.to_vec());
                        }
                    }
                    set
                };

                // Derive the BFT consensus P2P bootstrap list from the gossip P2P
                // bootstrap peers: validators reuse their identity key (same ed25519
                // pubkey) for both networks, but the BFT engine listens on
                // `gossip_port + 1`. Use *each peer's* gossip port + 1 (not the
                // local node's), since in setups where validators run on different
                // ports (e.g. a single-host devnet) every peer has its own offset.
                let bft_bootstrap_peers: Vec<(ed25519::PublicKey, std::net::SocketAddr)> = bootstrap
                    .iter()
                    .filter_map(|(peer_hex, addr)| {
                        let bytes = match hex::decode(peer_hex) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::warn!(peer = peer_hex, error = %e, "skipping BFT bootstrap peer: invalid hex");
                                return None;
                            }
                        };
                        if !validator_pubkeys.contains(&bytes) {
                            tracing::debug!(
                                peer = peer_hex,
                                "skipping non-validator gossip peer when building BFT bootstrap"
                            );
                            return None;
                        }
                        let pk = match ed25519::PublicKey::decode(&bytes[..]) {
                            Ok(pk) => pk,
                            Err(e) => {
                                tracing::warn!(peer = peer_hex, error = %e, "skipping BFT bootstrap peer: invalid ed25519 public key");
                                return None;
                            }
                        };
                        let mut bft_addr = *addr;
                        bft_addr.set_port(addr.port().saturating_add(1));
                        Some((pk, bft_addr))
                    })
                    .collect();
                info!(
                    bft_peers = bft_bootstrap_peers.len(),
                    consensus_p2p_port,
                    "BFT consensus P2P bootstrap peers configured"
                );

                let _handle = node.start_bft_engine(ed25519_key, consensus_p2p_port, bft_bootstrap_peers);
            }
        }
        NodeMode::Full | NodeMode::Archive => {
            // Full / archive nodes must NOT produce blocks. They follow the
            // canonical chain finalized by the validators by responding to
            // BlockAnnouncement broadcasts (which trigger SyncRequests) and by
            // applying SyncResponse payloads in the P2P receive loop. Running
            // `start_consensus_loop` here would have each full node
            // independently produce its own (divergent) blocks.
            info!("step 7: full/archive node — passive sync only, no block production");
        }
    }

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

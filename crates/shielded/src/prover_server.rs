//! HTTP prover server — accepts shielded proving requests over JSON/HTTP.
//!
//! Available when the `prover-server` feature is enabled.
//!
//! Endpoints:
//! - `POST /prove/deposit` — generate a deposit proof (auth + rate limit)
//! - `POST /prove/transfer` — generate a transfer proof (auth + rate limit)
//! - `POST /prove/withdraw` — generate a withdraw proof (auth + rate limit)
//! - `GET /health` — health check (no auth)

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tracing::{info, warn};

use crate::{
    circuit_deposit::{DepositCircuit, DepositWitness},
    circuit_transfer::{InputNoteWitness, OutputNoteWitness, TransferCircuit},
    circuit_withdraw::{WithdrawCircuit, WithdrawWitness},
    poseidon::{bytes_to_fp, domain, fp_to_bytes, poseidon_hash},
    prover::Halo2Prover,
    ViewingKey,
};

// ── Server State ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ProverState {
    pub prover: &'static Halo2Prover,
    pub mode: ProverMode,
    pub api_keys: Arc<HashSet<String>>,
    pub rate_limiter: Arc<Mutex<HashMap<String, TokenBucket>>>,
    pub max_qps: f64,
    pub proof_cache: Arc<Mutex<HashMap<[u8; 32], (Vec<u8>, Instant)>>>,
    pub cache_ttl_secs: u64,
    pub inflight: Arc<AtomicUsize>,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum ProverMode {
    Production,
    Dev,
}

impl ProverMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProverMode::Production => "production",
            ProverMode::Dev => "dev",
        }
    }
}

// ── Token Bucket Rate Limiter ─────────────────────────────────────────────

pub struct TokenBucket {
    tokens: f64,
    last_update: Instant,
    max_qps: f64,
}

impl TokenBucket {
    pub fn new(max_qps: f64) -> Self {
        Self {
            tokens: max_qps * 2.0, // burst capacity = 2x max_qps
            last_update: Instant::now(),
            max_qps,
        }
    }

    pub fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.max_qps).min(self.max_qps * 2.0);
        self.last_update = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ── Auth + Rate Limit Middleware ──────────────────────────────────────────

async fn auth_and_rate_limit(
    State(state): State<ProverState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    // If no API keys configured, allow all (dev mode)
    if state.api_keys.is_empty() {
        return Ok(next.run(request).await);
    }

    let api_key = headers
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let api_key = match api_key {
        Some(key) => key,
        None => {
            warn!("missing X-API-Key header");
            return Err((StatusCode::UNAUTHORIZED, "missing X-API-Key".into()));
        }
    };

    if !state.api_keys.contains(&api_key) {
        warn!("invalid X-API-Key");
        return Err((StatusCode::UNAUTHORIZED, "invalid X-API-Key".into()));
    }

    // Rate limit check
    {
        let mut limiter = state.rate_limiter.lock().expect("lock poisoned");
        let bucket = limiter
            .entry(api_key.clone())
            .or_insert_with(|| TokenBucket::new(state.max_qps));
        if !bucket.try_consume() {
            warn!(api_key = %api_key, "rate limit exceeded");
            return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded".into()));
        }
    }

    Ok(next.run(request).await)
}

// ── Request/Response Types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DepositRequest {
    pub value: u128,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
}

#[derive(Serialize)]
pub struct DepositResponse {
    pub proof: String,
    pub commitment: String,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
}

#[derive(Deserialize)]
pub struct TransferRequest {
    pub inputs: Vec<InputNoteRequest>,
    pub outputs: Vec<OutputNoteRequest>,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "merkleRoot")]
    pub merkle_root: String,
}

#[derive(Deserialize)]
pub struct InputNoteRequest {
    pub value: u128,
    pub rcm: String,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
    #[serde(rename = "spendingKey")]
    pub spending_key: String,
    #[serde(rename = "merklePath")]
    pub merkle_path: Vec<MerklePathEntry>,
}

#[derive(Deserialize)]
pub struct OutputNoteRequest {
    pub value: u128,
    pub rcm: String,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
}

#[derive(Deserialize)]
pub struct MerklePathEntry {
    pub sibling: String,
    #[serde(rename = "isRight")]
    pub is_right: bool,
}

#[derive(Serialize)]
pub struct TransferResponse {
    pub proof: String,
    pub nullifiers: Vec<String>,
    pub commitments: Vec<String>,
}

#[derive(Deserialize)]
pub struct WithdrawRequest {
    pub value: u128,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "targetAddress")]
    pub target_address: String,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    #[serde(rename = "spendingKey")]
    pub spending_key: String,
    pub rho: String,
    #[serde(rename = "merkleRoot")]
    pub merkle_root: String,
    #[serde(rename = "merklePath")]
    pub merkle_path: Vec<MerklePathEntry>,
}

#[derive(Serialize)]
pub struct WithdrawResponse {
    pub proof: String,
    pub nullifier: String,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub mode: &'static str,
    pub proving_key_loaded: bool,
    pub queue_depth: usize,
    pub cache_size: usize,
}

// ── Router ─────────────────────────────────────────────────────────────────

pub fn build_router(state: ProverState) -> Router {
    let protected = Router::new()
        .route("/prove/deposit", post(handle_deposit))
        .route("/prove/transfer", post(handle_transfer))
        .route("/prove/withdraw", post(handle_withdraw))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_and_rate_limit,
        ));

    Router::new()
        .route("/health", get(health_check))
        .merge(protected)
        .with_state(state)
}

// ── Health ─────────────────────────────────────────────────────────────────

async fn health_check(State(state): State<ProverState>) -> Json<HealthResponse> {
    let cache_size = state.proof_cache.lock().expect("lock poisoned").len();
    let queue_depth = state.inflight.load(Ordering::Relaxed);
    Json(HealthResponse {
        status: "ok",
        mode: state.mode.as_str(),
        proving_key_loaded: true, // Halo2Prover::global() would have panicked if setup failed
        queue_depth,
        cache_size,
    })
}

// ── Proof Cache Helpers ───────────────────────────────────────────────────

pub fn cache_get(
    cache: &mut HashMap<[u8; 32], (Vec<u8>, Instant)>,
    key: &[u8; 32],
    ttl_secs: u64,
) -> Option<Vec<u8>> {
    let now = Instant::now();
    if let Some((proof, ts)) = cache.get(key) {
        if now.duration_since(*ts).as_secs() < ttl_secs {
            return Some(proof.clone());
        }
    }
    None
}

pub fn cache_insert(
    cache: &mut HashMap<[u8; 32], (Vec<u8>, Instant)>,
    key: [u8; 32],
    proof: Vec<u8>,
) {
    cache.insert(key, (proof, Instant::now()));
}

// ── Deposit ────────────────────────────────────────────────────────────────

async fn handle_deposit(
    State(state): State<ProverState>,
    Json(req): Json<DepositRequest>,
) -> Result<Json<DepositResponse>, (StatusCode, String)> {
    let recipient_ivk = decode_hex_32(&req.recipient_ivk, "recipientIvk")?;
    let rho = decode_hex_32(&req.rho, "rho")?;

    let vk = ViewingKey {
        incoming_view_key: recipient_ivk,
        full_view_key: derive_fvk_from_ivk(&recipient_ivk),
    };

    // Compute rcm: poseidon_hash(["rcm", ivk, value, asset_id, rho])
    let rcm = compute_rcm_poseidon(&vk, req.value, req.asset_id, &rho);

    // Compute commitment: poseidon_hash([value, asset_id, rcm, rho])
    let commitment = compute_commitment_poseidon(req.value, req.asset_id, &rcm, &rho);

    // Cache key = nullifier(recipient_ivk, rho)
    let cache_key = compute_nullifier(&recipient_ivk, &rho);
    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        if let Some(cached_proof) = cache_get(&mut *cache, &cache_key, state.cache_ttl_secs) {
            info!("deposit proof cache hit");
            return Ok(Json(DepositResponse {
                proof: hex::encode(&cached_proof),
                commitment: hex::encode(commitment),
                asset_id: req.asset_id,
            }));
        }
    }

    let witness = DepositWitness {
        value: req.value,
        rcm,
        recipient_ivk,
        rho,
    };

    let circuit = DepositCircuit::new(commitment, req.asset_id, witness);

    state.inflight.fetch_add(1, Ordering::Relaxed);
    let proof_data = state.prover.prove_deposit(&circuit).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("prove failed: {e}"),
        )
    })?;
    state.inflight.fetch_sub(1, Ordering::Relaxed);

    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        cache_insert(&mut *cache, cache_key, proof_data.clone());
    }

    info!(
        asset_id = req.asset_id,
        value = req.value,
        "deposit proof generated"
    );

    Ok(Json(DepositResponse {
        proof: hex::encode(&proof_data),
        commitment: hex::encode(commitment),
        asset_id: req.asset_id,
    }))
}

// ── Transfer ───────────────────────────────────────────────────────────────

async fn handle_transfer(
    State(state): State<ProverState>,
    Json(req): Json<TransferRequest>,
) -> Result<Json<TransferResponse>, (StatusCode, String)> {
    let merkle_root = decode_hex_32(&req.merkle_root, "merkleRoot")?;

    // Build input witnesses
    let mut input_witnesses = Vec::with_capacity(req.inputs.len());
    let mut merkle_paths = Vec::with_capacity(req.inputs.len());
    let mut nullifiers = Vec::new();

    for input in &req.inputs {
        let spending_key = decode_hex_32(&input.spending_key, "spendingKey")?;
        let recipient_ivk = decode_hex_32(&input.recipient_ivk, "recipientIvk")?;
        let rho = decode_hex_32(&input.rho, "rho")?;
        let rcm = decode_hex_32(&input.rcm, "rcm")?;

        let nf = compute_nullifier(&recipient_ivk, &rho);
        nullifiers.push(nf);

        let merkle_path: Vec<([u8; 32], bool)> = input
            .merkle_path
            .iter()
            .map(|e| {
                let sibling =
                    decode_hex_32(&e.sibling, "merklePath.sibling").expect("valid merkle sibling");
                (sibling, e.is_right)
            })
            .collect();

        input_witnesses.push(InputNoteWitness {
            value: input.value,
            rcm,
            recipient_ivk,
            rho,
            spending_key,
        });
        merkle_paths.push(merkle_path);
    }

    // Build output witnesses and commitments
    let mut output_witnesses = Vec::with_capacity(req.outputs.len());
    let mut commitments = Vec::new();

    for output in &req.outputs {
        let recipient_ivk = decode_hex_32(&output.recipient_ivk, "recipientIvk")?;
        let rho = decode_hex_32(&output.rho, "rho")?;
        let rcm = decode_hex_32(&output.rcm, "rcm")?;

        let _vk = ViewingKey {
            incoming_view_key: recipient_ivk,
            full_view_key: derive_fvk_from_ivk(&recipient_ivk),
        };

        let cm = compute_commitment_poseidon(output.value, req.asset_id, &rcm, &rho);
        commitments.push(cm);

        output_witnesses.push(OutputNoteWitness {
            value: output.value,
            rcm,
            recipient_ivk,
            rho,
        });
    }

    // Cache key = keccak256 of (nullifiers + commitments + merkle_root)
    let cache_key = {
        let mut key_data = Vec::with_capacity(nullifiers.len() * 32 + commitments.len() * 32 + 32);
        for nf in &nullifiers {
            key_data.extend_from_slice(nf);
        }
        for cm in &commitments {
            key_data.extend_from_slice(cm);
        }
        key_data.extend_from_slice(&merkle_root);
        alloy_primitives::keccak256(&key_data).0
    };
    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        if let Some(cached_proof) = cache_get(&mut *cache, &cache_key, state.cache_ttl_secs) {
            info!("transfer proof cache hit");
            let nf_hex: Vec<String> = nullifiers.iter().map(hex::encode).collect();
            let cm_hex: Vec<String> = commitments.iter().map(hex::encode).collect();
            return Ok(Json(TransferResponse {
                proof: hex::encode(&cached_proof),
                nullifiers: nf_hex,
                commitments: cm_hex,
            }));
        }
    }

    // Pre-compute hex strings before moving vectors into circuit
    let nf_hex: Vec<String> = nullifiers.iter().map(hex::encode).collect();
    let cm_hex: Vec<String> = commitments.iter().map(hex::encode).collect();

    // TransferCircuit requires exactly 2 inputs and 2 outputs (fixed-size arrays).
    // Reject if more than 2, pad with dummy zero-value notes if fewer.
    if input_witnesses.len() > 2 || output_witnesses.len() > 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            "transfer supports at most 2 inputs and 2 outputs".into(),
        ));
    }
    while input_witnesses.len() < 2 {
        input_witnesses.push(InputNoteWitness {
            value: 0,
            rcm: [0u8; 32],
            recipient_ivk: [0u8; 32],
            rho: [0u8; 32],
            spending_key: [0u8; 32],
        });
        nullifiers.push([0u8; 32]);
        merkle_paths.push(vec![([0u8; 32], false); 32]);
    }
    while output_witnesses.len() < 2 {
        output_witnesses.push(OutputNoteWitness {
            value: 0,
            rcm: [0u8; 32],
            recipient_ivk: [0u8; 32],
            rho: [0u8; 32],
        });
        commitments.push([0u8; 32]);
    }

    // Convert Vec merkle paths to fixed-size arrays
    let mut merkle_paths_arr: [[([u8; 32], bool); 32]; 2] = [[([0u8; 32], false); 32]; 2];
    for (i, path) in merkle_paths.iter().enumerate() {
        for (j, entry) in path.iter().enumerate() {
            merkle_paths_arr[i][j] = *entry;
        }
    }

    let circuit = TransferCircuit::new(
        [nullifiers[0], nullifiers[1]],
        [commitments[0], commitments[1]],
        req.asset_id,
        merkle_root,
        [input_witnesses[0], input_witnesses[1]],
        [output_witnesses[0], output_witnesses[1]],
        merkle_paths_arr,
    );

    state.inflight.fetch_add(1, Ordering::Relaxed);
    let proof_data = state.prover.prove_transfer(&circuit).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("prove failed: {e}"),
        )
    })?;
    state.inflight.fetch_sub(1, Ordering::Relaxed);

    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        cache_insert(&mut *cache, cache_key, proof_data.clone());
    }

    info!(
        inputs = req.inputs.len(),
        outputs = req.outputs.len(),
        asset_id = req.asset_id,
        "transfer proof generated"
    );

    Ok(Json(TransferResponse {
        proof: hex::encode(&proof_data),
        nullifiers: nf_hex,
        commitments: cm_hex,
    }))
}

// ── Withdraw ───────────────────────────────────────────────────────────────

async fn handle_withdraw(
    State(state): State<ProverState>,
    Json(req): Json<WithdrawRequest>,
) -> Result<Json<WithdrawResponse>, (StatusCode, String)> {
    let recipient_ivk = decode_hex_32(&req.recipient_ivk, "recipientIvk")?;
    let spending_key = decode_hex_32(&req.spending_key, "spendingKey")?;
    let rho = decode_hex_32(&req.rho, "rho")?;
    let target_address = decode_hex_20(&req.target_address, "targetAddress")?;
    let merkle_root = decode_hex_32(&req.merkle_root, "merkleRoot")?;

    let vk = ViewingKey {
        incoming_view_key: recipient_ivk,
        full_view_key: derive_fvk_from_ivk(&recipient_ivk),
    };

    let rcm = compute_rcm_poseidon(&vk, req.value, req.asset_id, &rho);
    let nullifier = compute_nullifier(&recipient_ivk, &rho);
    let _commitment = compute_commitment_poseidon(req.value, req.asset_id, &rcm, &rho);

    // Cache key = nullifier
    let cache_key = nullifier;
    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        if let Some(cached_proof) = cache_get(&mut *cache, &cache_key, state.cache_ttl_secs) {
            info!("withdraw proof cache hit");
            return Ok(Json(WithdrawResponse {
                proof: hex::encode(&cached_proof),
                nullifier: hex::encode(nullifier),
            }));
        }
    }

    let merkle_path: Vec<([u8; 32], bool)> = req
        .merkle_path
        .iter()
        .map(|e| {
            let sibling =
                decode_hex_32(&e.sibling, "merklePath.sibling").expect("valid merkle sibling");
            (sibling, e.is_right)
        })
        .collect();

    let witness = WithdrawWitness {
        note_value: req.value,
        rcm,
        recipient_ivk,
        rho,
        spending_key,
        merkle_path,
    };

    let mut target_addr = [0u8; 20];
    target_addr.copy_from_slice(&target_address);

    let circuit = WithdrawCircuit::new(
        nullifier,
        req.asset_id,
        req.value,
        target_addr,
        merkle_root,
        witness,
    );

    state.inflight.fetch_add(1, Ordering::Relaxed);
    let proof_data = state.prover.prove_withdraw(&circuit).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("prove failed: {e}"),
        )
    })?;
    state.inflight.fetch_sub(1, Ordering::Relaxed);

    {
        let mut cache = state.proof_cache.lock().expect("lock poisoned");
        cache_insert(&mut *cache, cache_key, proof_data.clone());
    }

    info!(
        value = req.value,
        asset_id = req.asset_id,
        "withdraw proof generated"
    );

    Ok(Json(WithdrawResponse {
        proof: hex::encode(&proof_data),
        nullifier: hex::encode(nullifier),
    }))
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub fn decode_hex_32(s: &str, field: &str) -> Result<[u8; 32], (StatusCode, String)> {
    let cleaned = s.trim_start_matches("0x");
    let bytes = hex::decode(cleaned)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid {field}: {e}")))?;
    if bytes.len() != 32 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} must be 32 bytes, got {}", bytes.len()),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

pub fn decode_hex_20(s: &str, field: &str) -> Result<[u8; 20], (StatusCode, String)> {
    let cleaned = s.trim_start_matches("0x");
    let bytes = hex::decode(cleaned)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid {field}: {e}")))?;
    if bytes.len() != 20 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} must be 20 bytes, got {}", bytes.len()),
        ));
    }
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Derive FVK from IVK using keccak256 (matching ViewingKey::generate).
pub fn derive_fvk_from_ivk(ivk: &[u8; 32]) -> [u8; 32] {
    let mut fvk_data = Vec::with_capacity(36);
    fvk_data.extend_from_slice(b"fvk");
    fvk_data.extend_from_slice(ivk);
    alloy_primitives::keccak256(&fvk_data).0
}

/// Compute RCM using Poseidon hash (matching circuit D3 constraint).
pub fn compute_rcm_poseidon(
    vk: &ViewingKey,
    value: u128,
    asset_id: u64,
    rho: &[u8; 32],
) -> [u8; 32] {
    let rcm_tag = domain_tag_to_fp(domain::RCM);
    let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
    let value_fp = value_to_fp_bytes(value);
    let asset_fp = asset_id_to_fp_bytes(asset_id);
    let rho_fp = bytes_to_fp(rho);
    let rcm_fp = poseidon_hash(&[rcm_tag, ivk_fp, value_fp, asset_fp, rho_fp]);
    fp_to_bytes(&rcm_fp)
}

/// Compute note commitment using Poseidon hash.
pub fn compute_commitment_poseidon(
    value: u128,
    asset_id: u64,
    rcm: &[u8; 32],
    rho: &[u8; 32],
) -> [u8; 32] {
    let value_fp = value_to_fp_bytes(value);
    let asset_fp = asset_id_to_fp_bytes(asset_id);
    let rcm_fp = bytes_to_fp(rcm);
    let rho_fp = bytes_to_fp(rho);
    let cm_fp = poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp]);
    fp_to_bytes(&cm_fp)
}

/// Compute nullifier using Poseidon hash (matching ViewingKey::derive_nullifier).
pub fn compute_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
    let domain_bytes = domain_tag_to_fp_bytes(domain::FVK_FROM_IVK);
    let fvk_tag = bytes_to_fp(&domain_bytes);
    let ivk_fp = bytes_to_fp(ivk);
    let rho_fp = bytes_to_fp(rho);
    let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fp]);
    let nf_fp = poseidon_hash(&[fvk_from_ivk, rho_fp]);
    fp_to_bytes(&nf_fp)
}

pub fn value_to_fp_bytes(value: u128) -> pasta_curves::Fp {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&value.to_le_bytes());
    bytes_to_fp(&bytes)
}

pub fn asset_id_to_fp_bytes(asset_id: u64) -> pasta_curves::Fp {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
    bytes_to_fp(&bytes)
}

pub fn domain_tag_to_fp(tag: &str) -> pasta_curves::Fp {
    let mut bytes = [0u8; 32];
    let tag_bytes = tag.as_bytes();
    let len = tag_bytes.len().min(32);
    bytes[..len].copy_from_slice(&tag_bytes[..len]);
    bytes_to_fp(&bytes)
}

pub fn domain_tag_to_fp_bytes(tag: &str) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let tag_bytes = tag.as_bytes();
    let len = tag_bytes.len().min(32);
    bytes[..len].copy_from_slice(&tag_bytes[..len]);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_token_bucket_allows_within_capacity() {
        let mut bucket = TokenBucket::new(10.0);
        // Burst capacity = 20, so 20 requests should succeed immediately
        for _ in 0..20 {
            assert!(bucket.try_consume());
        }
        // 21st should fail
        assert!(!bucket.try_consume());
    }

    #[test]
    fn test_token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(10.0);
        // Consume all tokens
        for _ in 0..20 {
            assert!(bucket.try_consume());
        }
        assert!(!bucket.try_consume());

        // Manually wind back last_update to simulate time passing
        bucket.last_update = Instant::now() - Duration::from_secs_f64(0.15);
        // 0.15s * 10 qps = 1.5 tokens replenished, so 1 request should succeed
        assert!(bucket.try_consume());
        assert!(!bucket.try_consume());
    }

    #[test]
    fn test_cache_hit_and_miss() {
        let mut cache: HashMap<[u8; 32], (Vec<u8>, Instant)> = HashMap::new();
        let key = [1u8; 32];
        let proof = vec![0xAB, 0xCD];

        // Miss before insert
        assert!(cache_get(&mut cache, &key, 300).is_none());

        // Insert
        cache_insert(&mut cache, key, proof.clone());

        // Hit after insert
        assert_eq!(cache_get(&mut cache, &key, 300), Some(proof));
    }

    #[test]
    fn test_cache_expires_after_ttl() {
        let mut cache: HashMap<[u8; 32], (Vec<u8>, Instant)> = HashMap::new();
        let key = [1u8; 32];

        cache.insert(key, (vec![0xAB], Instant::now() - Duration::from_secs(400)));

        // TTL = 300s, entry is 400s old => expired
        assert!(cache_get(&mut cache, &key, 300).is_none());
    }

    #[test]
    fn test_decode_hex_32_valid() {
        let result = decode_hex_32(
            "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            "test",
        );
        assert!(result.is_ok());
        let arr = result.unwrap();
        assert_eq!(arr[0], 0x12);
        assert_eq!(arr[31], 0xef);
    }

    #[test]
    fn test_decode_hex_32_wrong_length() {
        let result = decode_hex_32("0x1234", "test");
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_decode_hex_32_invalid_hex() {
        let result = decode_hex_32("not-hex", "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_hex_20_valid() {
        let result = decode_hex_20("0x1234567890abcdef1234567890abcdef12345678", "test");
        assert!(result.is_ok());
        let arr = result.unwrap();
        assert_eq!(arr.len(), 20);
        assert_eq!(arr[0], 0x12);
        assert_eq!(arr[19], 0x78);
    }

    #[test]
    fn test_decode_hex_20_wrong_length() {
        let result = decode_hex_20("0x1234", "test");
        assert!(result.is_err());
    }
}

//! HTTP prover server — accepts shielded proving requests over JSON/HTTP.
//!
//! Endpoints:
//! - `POST /prove/deposit` — generate a deposit proof
//! - `POST /prove/transfer` — generate a transfer proof
//! - `POST /prove/withdraw` — generate a withdraw proof
//! - `GET /health` — health check

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use call_crypto::keccak256;
use call_shielded::{
    ViewingKey,
    circuit_deposit::{DepositCircuit, DepositWitness},
    circuit_withdraw::{WithdrawCircuit, WithdrawWitness},
    circuit_transfer::{InputNoteWitness, OutputNoteWitness, TransferCircuit},
    poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, domain},
    prover::RealProver,
};
use serde::{Deserialize, Serialize};
use tracing::info;

// ── Server State ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct ProverState {
    pub prover: &'static RealProver,
    pub mode: ProverMode,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum ProverMode {
    Production,
    Dev,
}

impl ProverMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ProverMode::Production => "production",
            ProverMode::Dev => "dev",
        }
    }
}

// ── Request/Response Types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct DepositRequest {
    pub value: u128,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
}

#[derive(Serialize)]
pub(crate) struct DepositResponse {
    pub proof: String,
    pub commitment: String,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
}

#[derive(Deserialize)]
pub(crate) struct TransferRequest {
    pub inputs: Vec<InputNoteRequest>,
    pub outputs: Vec<OutputNoteRequest>,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "merkleRoot")]
    pub merkle_root: String,
}

#[derive(Deserialize)]
pub(crate) struct InputNoteRequest {
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
pub(crate) struct OutputNoteRequest {
    pub value: u128,
    pub rcm: String,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
}

#[derive(Deserialize)]
pub(crate) struct MerklePathEntry {
    pub sibling: String,
    #[serde(rename = "isRight")]
    pub is_right: bool,
}

#[derive(Serialize)]
pub(crate) struct TransferResponse {
    pub proof: String,
    pub nullifiers: Vec<String>,
    pub commitments: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct WithdrawRequest {
    pub value: u128,
    #[serde(rename = "assetId")]
    pub asset_id: u64,
    #[serde(rename = "targetAddress")]
    pub target_address: String,
    #[serde(rename = "recipientIvk")]
    pub recipient_ivk: String,
    pub rho: String,
    #[serde(rename = "merkleRoot")]
    pub merkle_root: String,
    #[serde(rename = "merklePath")]
    pub merkle_path: Vec<MerklePathEntry>,
}

#[derive(Serialize)]
pub(crate) struct WithdrawResponse {
    pub proof: String,
    pub nullifier: String,
}

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    pub status: &'static str,
    pub mode: &'static str,
}

// ── Router ─────────────────────────────────────────────────────────────────

pub(crate) fn build_router(state: ProverState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/prove/deposit", post(handle_deposit))
        .route("/prove/transfer", post(handle_transfer))
        .route("/prove/withdraw", post(handle_withdraw))
        .with_state(state)
}

// ── Health ─────────────────────────────────────────────────────────────────

async fn health_check(State(state): State<ProverState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        mode: state.mode.as_str(),
    })
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

    let witness = DepositWitness {
        value: req.value,
        rcm,
        recipient_ivk,
        rho,
    };

    let circuit = DepositCircuit::new(commitment, req.asset_id, witness);

    let proof_data = state
        .prover
        .prove_deposit(&circuit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("prove failed: {e}")))?;

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
                let sibling = decode_hex_32(&e.sibling, "merklePath.sibling")
                    .expect("valid merkle sibling");
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

    // Pre-compute hex strings before moving vectors into circuit
    let nf_hex: Vec<String> = nullifiers.iter().map(hex::encode).collect();
    let cm_hex: Vec<String> = commitments.iter().map(hex::encode).collect();

    // Build nullifier and commitment lists as raw [u8; 32]
    let nf_list: Vec<[u8; 32]> = nullifiers;
    let cm_list: Vec<[u8; 32]> = commitments;

    let circuit = TransferCircuit::new(
        nf_list,
        cm_list,
        req.asset_id,
        merkle_root,
        input_witnesses,
        output_witnesses,
        merkle_paths,
    );

    let proof_data = state
        .prover
        .prove_transfer(&circuit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("prove failed: {e}")))?;

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

    let proof_data = state
        .prover
        .prove_withdraw(&circuit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("prove failed: {e}")))?;

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

fn decode_hex_32(s: &str, field: &str) -> Result<[u8; 32], (StatusCode, String)> {
    let cleaned = s.trim_start_matches("0x");
    let bytes =
        hex::decode(cleaned).map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid {field}: {e}")))?;
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

fn decode_hex_20(s: &str, field: &str) -> Result<[u8; 20], (StatusCode, String)> {
    let cleaned = s.trim_start_matches("0x");
    let bytes =
        hex::decode(cleaned).map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid {field}: {e}")))?;
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
fn derive_fvk_from_ivk(ivk: &[u8; 32]) -> [u8; 32] {
    let mut fvk_data = Vec::with_capacity(36);
    fvk_data.extend_from_slice(b"fvk");
    fvk_data.extend_from_slice(ivk);
    keccak256(&fvk_data).0
}

/// Compute RCM using Poseidon hash (matching circuit D3 constraint).
fn compute_rcm_poseidon(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
    let rcm_tag = domain_tag_to_fr("rcm");
    let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
    let value_fr = value_to_fr_bytes(value);
    let asset_fr = asset_id_to_fr_bytes(asset_id);
    let rho_fr = bytes_to_fr(rho);
    let rcm_fr = poseidon_hash(&[rcm_tag, ivk_fr, value_fr, asset_fr, rho_fr]);
    fr_to_bytes(&rcm_fr)
}

/// Compute note commitment using Poseidon hash.
fn compute_commitment_poseidon(
    value: u128,
    asset_id: u64,
    rcm: &[u8; 32],
    rho: &[u8; 32],
) -> [u8; 32] {
    let value_fr = value_to_fr_bytes(value);
    let asset_fr = asset_id_to_fr_bytes(asset_id);
    let rcm_fr = bytes_to_fr(rcm);
    let rho_fr = bytes_to_fr(rho);
    let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
    fr_to_bytes(&cm_fr)
}

/// Compute nullifier using Poseidon hash (matching ViewingKey::derive_nullifier).
fn compute_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
    let domain_bytes = domain_tag_to_fr_bytes(domain::FVK_FROM_IVK);
    let fvk_tag = bytes_to_fr(&domain_bytes);
    let ivk_fr = bytes_to_fr(ivk);
    let rho_fr = bytes_to_fr(rho);
    let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
    let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
    fr_to_bytes(&nf_fr)
}

fn value_to_fr_bytes(value: u128) -> ark_bn254::Fr {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&value.to_le_bytes());
    bytes_to_fr(&bytes)
}

fn asset_id_to_fr_bytes(asset_id: u64) -> ark_bn254::Fr {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
    bytes_to_fr(&bytes)
}

fn domain_tag_to_fr(tag: &str) -> ark_bn254::Fr {
    let mut bytes = [0u8; 32];
    let tag_bytes = tag.as_bytes();
    let len = tag_bytes.len().min(32);
    bytes[..len].copy_from_slice(&tag_bytes[..len]);
    bytes_to_fr(&bytes)
}

fn domain_tag_to_fr_bytes(tag: &str) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let tag_bytes = tag.as_bytes();
    let len = tag_bytes.len().min(32);
    bytes[..len].copy_from_slice(&tag_bytes[..len]);
    bytes
}

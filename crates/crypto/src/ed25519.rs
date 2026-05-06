//! Ed25519: key generation, signing, verification (for consensus)

use call_primitives::Ed25519PublicKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Ed25519Error {
    #[error("invalid signature")]
    InvalidSignature,
    #[error("verification failed")]
    VerificationFailed,
}

/// Generate a new Ed25519 keypair, returning (public_key, signing_key)
pub fn ed25519_generate_keypair() -> (Ed25519PublicKey, SigningKey) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    (verifying_key.to_bytes(), signing_key)
}

/// Sign a message with an Ed25519 signing key.
pub fn ed25519_sign(signing_key: &SigningKey, message: &[u8]) -> [u8; 64] {
    signing_key.sign(message).to_bytes()
}

/// Verify an Ed25519 signature against a public key and message.
pub fn ed25519_verify(
    public_key: &Ed25519PublicKey,
    signature: &[u8; 64],
    message: &[u8],
) -> Result<(), Ed25519Error> {
    let verifying_key =
        VerifyingKey::from_bytes(public_key).map_err(|_| Ed25519Error::InvalidSignature)?;
    let sig = Signature::from_bytes(signature);
    verifying_key
        .verify(message, &sig)
        .map_err(|_| Ed25519Error::VerificationFailed)
}

// ── VRF (Verifiable Random Function) using Ed25519 ────────────────────

/// Domain separator for VRF proposer sortition.
const VRF_DOMAIN: &[u8] = b"CALLCHAIN-PROPOSER-VRF-v1";

/// Produce a VRF proof and output for the given seed.
///
/// Returns `(signature, output_hash)` where:
/// - `signature` is the Ed25519 proof (64 bytes)
/// - `output_hash` is `keccak256(signature)` used as the lottery ticket
pub fn vrf_prove(signing_key: &SigningKey, seed: &[u8; 32]) -> ([u8; 64], [u8; 32]) {
    let mut message = Vec::with_capacity(VRF_DOMAIN.len() + 32);
    message.extend_from_slice(VRF_DOMAIN);
    message.extend_from_slice(seed);
    let sig = ed25519_sign(signing_key, &message);
    let output = crate::keccak256(&sig);
    (sig, output.into())
}

/// Verify a VRF proof and return the output hash.
///
/// Returns `Some(output_hash)` if the signature is valid, `None` otherwise.
pub fn vrf_verify(
    public_key: &Ed25519PublicKey,
    seed: &[u8; 32],
    signature: &[u8; 64],
) -> Option<[u8; 32]> {
    let mut message = Vec::with_capacity(VRF_DOMAIN.len() + 32);
    message.extend_from_slice(VRF_DOMAIN);
    message.extend_from_slice(seed);
    ed25519_verify(public_key, signature, &message).ok()?;
    Some(crate::keccak256(signature).into())
}

/// Compute a deterministic VRF sortition score for a validator.
///
/// This is a *simplified* VRF: the score is derived from the validator's
/// public key and an unbiasable seed (e.g. previous block hash + epoch).
/// It does not require a secret key, making it suitable for off-chain
/// subset computation where all participants can verify the result.
///
/// For true unpredictability, use `vrf_prove` / `vrf_verify`.
pub fn vrf_sortition_score(validator_pubkey: &Ed25519PublicKey, seed: &[u8; 32]) -> [u8; 32] {
    let mut data = Vec::with_capacity(VRF_DOMAIN.len() + 32 + 32);
    data.extend_from_slice(VRF_DOMAIN);
    data.extend_from_slice(seed);
    data.extend_from_slice(validator_pubkey);
    crate::keccak256(&data).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ed25519_sign_verify() {
        let (pubkey, signing_key) = ed25519_generate_keypair();
        let message = b"consensus message";
        let sig = ed25519_sign(&signing_key, message);
        assert!(ed25519_verify(&pubkey, &sig, message).is_ok());

        // Wrong message should fail
        assert!(ed25519_verify(&pubkey, &sig, b"wrong message").is_err());
    }
}

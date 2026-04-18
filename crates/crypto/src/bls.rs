//! BLS12-381 signature support for consensus vote compression
//!
//! Using the `blst` library (same as Ethereum), with min_pk variant:
//! - Public keys: 48 bytes (compressed)
//! - Signatures: 96 bytes (compressed)

use blst::{min_pk::PublicKey as BlstPublicKey, BLST_ERROR};
use rand_core::OsRng;
use rand_core::RngCore;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BlsError {
    #[error("invalid secret key")]
    InvalidSecretKey,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("signature verification failed")]
    VerificationFailed,
}

/// BLS12-381 public key (48 bytes compressed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlsPublicKey(pub [u8; 48]);

/// BLS12-381 signature (96 bytes compressed)
#[derive(Debug, Clone, Copy)]
pub struct BlsSignature(pub [u8; 96]);

/// BLS12-381 secret key
pub struct BlsSecretKey {
    inner: blst::min_pk::SecretKey,
}

/// Domain Separation Tag for consensus signing
const DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// Generate a new BLS12-381 keypair
pub fn bls_generate() -> Result<(BlsSecretKey, BlsPublicKey), BlsError> {
    let mut ikm = [0u8; 32];
    OsRng.fill_bytes(&mut ikm);

    let secret = blst::min_pk::SecretKey::key_gen(&ikm, &[])
        .map_err(|_| BlsError::InvalidSecretKey)?;
    let public = BlsPublicKey(secret.sk_to_pk().to_bytes());

    Ok((BlsSecretKey { inner: secret }, public))
}

/// Sign a message with BLS12-381
pub fn bls_sign(secret: &BlsSecretKey, msg: &[u8]) -> BlsSignature {
    let sig = secret.inner.sign(msg, DST, &[]);
    BlsSignature(sig.to_bytes())
}

/// Verify a single BLS signature
pub fn bls_verify(
    pubkey: &BlsPublicKey,
    msg: &[u8],
    sig: &BlsSignature,
) -> Result<(), BlsError> {
    let pk = BlstPublicKey::uncompress(&pubkey.0)
        .map_err(|_| BlsError::InvalidPublicKey)?;
    let sig = blst::min_pk::Signature::uncompress(&sig.0)
        .map_err(|_| BlsError::InvalidSignature)?;

    let err = sig.verify(true, msg, DST, &[], &pk, true);
    if err == BLST_ERROR::BLST_SUCCESS {
        Ok(())
    } else {
        Err(BlsError::VerificationFailed)
    }
}

/// Compress a BLS signature to bytes
pub fn bls_signature_bytes(sig: &BlsSignature) -> [u8; 96] {
    sig.0
}

/// Decompress a BLS signature from bytes
pub fn bls_signature_from_bytes(bytes: [u8; 96]) -> BlsSignature {
    BlsSignature(bytes)
}

/// Compress a BLS public key to bytes
pub fn bls_public_key_bytes(pk: &BlsPublicKey) -> [u8; 48] {
    pk.0
}

/// Decompress a BLS public key from bytes
pub fn bls_public_key_from_bytes(bytes: [u8; 48]) -> Result<BlsPublicKey, BlsError> {
    BlstPublicKey::uncompress(&bytes)
        .map(|_| BlsPublicKey(bytes))
        .map_err(|_| BlsError::InvalidPublicKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bls_sign_verify() {
        let (secret, pubkey) = bls_generate().unwrap();
        let msg = b"consensus vote for block 42";
        let sig = bls_sign(&secret, msg);

        assert!(bls_verify(&pubkey, msg, &sig).is_ok());
        assert!(bls_verify(&pubkey, b"wrong message", &sig).is_err());
    }

    #[test]
    fn test_bls_key_roundtrip() {
        let (_, pk) = bls_generate().unwrap();
        let bytes = bls_public_key_bytes(&pk);
        let restored = bls_public_key_from_bytes(bytes).unwrap();
        assert_eq!(pk, restored);
    }

    #[test]
    fn test_bls_signature_roundtrip() {
        let (secret, _) = bls_generate().unwrap();
        let msg = b"test";
        let sig = bls_sign(&secret, msg);
        let bytes = bls_signature_bytes(&sig);
        let restored = bls_signature_from_bytes(bytes);
        assert_eq!(sig.0, restored.0);
    }
}

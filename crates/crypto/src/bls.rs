//! BLS12-381 signature support for consensus vote compression
//!
//! Using the `blst` library (same as Ethereum), with min_pk variant:
//! - Public keys: 48 bytes (compressed)
//! - Signatures: 96 bytes (compressed)

use blst::{
    min_pk::{AggregateSignature, PublicKey as BlstPublicKey},
    BLST_ERROR,
};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlsSignature(pub [u8; 96]);

/// BLS12-381 secret key
#[derive(Clone)]
pub struct BlsSecretKey {
    inner: blst::min_pk::SecretKey,
}

/// Domain Separation Tag for consensus signing
const DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// Domain Separation Tag for Ethereum beacon chain signing (proof-of-possession).
///
/// The beacon chain uses the `POP` variant instead of `NUL` for all BLS operations
/// within the consensus layer (sync committee signatures, randao, etc.).
const DST_BEACON: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_";

/// Generate a new BLS12-381 keypair
pub fn bls_generate() -> Result<(BlsSecretKey, BlsPublicKey), BlsError> {
    let mut ikm = [0u8; 32];
    OsRng.fill_bytes(&mut ikm);

    let secret =
        blst::min_pk::SecretKey::key_gen(&ikm, &[]).map_err(|_| BlsError::InvalidSecretKey)?;
    let public = BlsPublicKey(secret.sk_to_pk().to_bytes());

    Ok((BlsSecretKey { inner: secret }, public))
}

/// Sign a message with BLS12-381
pub fn bls_sign(secret: &BlsSecretKey, msg: &[u8]) -> BlsSignature {
    let sig = secret.inner.sign(msg, DST, &[]);
    BlsSignature(sig.to_bytes())
}

/// Sign a message with BLS12-381 using the Ethereum beacon chain DST.
///
/// This is the counterpart to [`bls_verify_aggregate_beacon`] and must be
/// used when producing signatures that will be verified by the beacon chain
/// sync committee verifier.
pub fn bls_sign_beacon(secret: &BlsSecretKey, msg: &[u8]) -> BlsSignature {
    let sig = secret.inner.sign(msg, DST_BEACON, &[]);
    BlsSignature(sig.to_bytes())
}

/// Verify a single BLS signature
pub fn bls_verify(pubkey: &BlsPublicKey, msg: &[u8], sig: &BlsSignature) -> Result<(), BlsError> {
    let pk = BlstPublicKey::uncompress(&pubkey.0).map_err(|_| BlsError::InvalidPublicKey)?;
    let sig =
        blst::min_pk::Signature::uncompress(&sig.0).map_err(|_| BlsError::InvalidSignature)?;

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

/// Aggregate multiple BLS signatures into one 96-byte signature.
/// Returns error if the input slice is empty or any signature is invalid.
pub fn bls_aggregate(sigs: &[BlsSignature]) -> Result<BlsSignature, BlsError> {
    if sigs.is_empty() {
        return Err(BlsError::InvalidSignature);
    }
    let first =
        blst::min_pk::Signature::uncompress(&sigs[0].0).map_err(|_| BlsError::InvalidSignature)?;
    let mut agg = AggregateSignature::from_signature(&first);
    for sig in &sigs[1..] {
        let s =
            blst::min_pk::Signature::uncompress(&sig.0).map_err(|_| BlsError::InvalidSignature)?;
        agg.add_signature(&s, true)
            .map_err(|_| BlsError::InvalidSignature)?;
    }
    Ok(BlsSignature(agg.to_signature().to_bytes()))
}

/// Verify an aggregated BLS signature against all pubkeys that contributed.
/// `pubkeys` must contain the pubkeys of every signer that produced a partial
/// signature included in the aggregate.
pub fn bls_verify_aggregate(
    pubkeys: &[BlsPublicKey],
    msg: &[u8],
    agg_sig: &BlsSignature,
) -> Result<(), BlsError> {
    bls_verify_aggregate_with_dst(pubkeys, msg, agg_sig, DST)
}

/// Verify an aggregated BLS signature using the Ethereum beacon chain DST.
///
/// This is the same as [`bls_verify_aggregate`] but uses the beacon chain's
/// proof-of-possession DST, which is required for sync committee signatures.
pub fn bls_verify_aggregate_beacon(
    pubkeys: &[BlsPublicKey],
    msg: &[u8],
    agg_sig: &BlsSignature,
) -> Result<(), BlsError> {
    bls_verify_aggregate_with_dst(pubkeys, msg, agg_sig, DST_BEACON)
}

fn bls_verify_aggregate_with_dst(
    pubkeys: &[BlsPublicKey],
    msg: &[u8],
    agg_sig: &BlsSignature,
    dst: &[u8],
) -> Result<(), BlsError> {
    let pks: Vec<BlstPublicKey> = pubkeys
        .iter()
        .map(|pk| BlstPublicKey::uncompress(&pk.0).map_err(|_| BlsError::InvalidPublicKey))
        .collect::<Result<Vec<_>, _>>()?;
    let pk_refs: Vec<&BlstPublicKey> = pks.iter().collect();

    let sig =
        blst::min_pk::Signature::uncompress(&agg_sig.0).map_err(|_| BlsError::InvalidSignature)?;

    let err = sig.fast_aggregate_verify(true, msg, dst, &pk_refs);
    if err == BLST_ERROR::BLST_SUCCESS {
        Ok(())
    } else {
        Err(BlsError::VerificationFailed)
    }
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

    #[test]
    fn test_bls_aggregate() {
        let msg = b"aggregate vote";
        let (sk1, pk1) = bls_generate().unwrap();
        let (sk2, pk2) = bls_generate().unwrap();
        let (sk3, pk3) = bls_generate().unwrap();

        let sig1 = bls_sign(&sk1, msg);
        let sig2 = bls_sign(&sk2, msg);
        let sig3 = bls_sign(&sk3, msg);

        let agg = bls_aggregate(&[sig1, sig2, sig3]).unwrap();
        assert_eq!(agg.0.len(), 96);

        // Verify aggregate against all 3 pubkeys
        assert!(bls_verify_aggregate(&[pk1, pk2, pk3], msg, &agg).is_ok());
    }

    #[test]
    fn test_bls_aggregate_wrong_message() {
        let msg = b"right message";
        let (sk1, pk1) = bls_generate().unwrap();
        let (sk2, pk2) = bls_generate().unwrap();

        let sig1 = bls_sign(&sk1, msg);
        let sig2 = bls_sign(&sk2, msg);
        let agg = bls_aggregate(&[sig1, sig2]).unwrap();

        // Verify against wrong message should fail
        assert!(bls_verify_aggregate(&[pk1, pk2], b"wrong message", &agg).is_err());
    }

    #[test]
    fn test_bls_aggregate_empty_fails() {
        assert!(bls_aggregate(&[]).is_err());
    }
}

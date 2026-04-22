//! secp256k1: key generation, signing, verification, address recovery

use call_primitives::{Address, PublicKey, Signature};
use k256::ecdsa::{VerifyingKey, Signature as K256Signature, RecoveryId};
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::SigningKey;
use rand::rngs::OsRng;
use sha3::{Digest, Keccak256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Secp256k1Error {
    #[error("invalid signature")]
    InvalidSignature,
    #[error("recovery failed")]
    RecoveryFailed,
    #[error("signature verification failed")]
    VerificationFailed,
}

/// Generate a new secp256k1 keypair, returning (secret_key_bytes, public_key_bytes)
pub fn generate_keypair() -> ([u8; 32], PublicKey) {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let encoded = verifying_key.to_encoded_point(false);
    let mut pubkey = [0u8; 64];
    pubkey.copy_from_slice(&encoded.as_bytes()[1..]); // skip 0x04 prefix
    let secret_bytes = *signing_key.to_bytes().as_ref();
    (secret_bytes, pubkey)
}

/// Sign a 32-byte message hash with the given secret key.
/// Returns a 65-byte signature (r || s || v).
pub fn secp256k1_sign(secret_key: &[u8; 32], msg_hash: &[u8; 32]) -> Signature {
    let signing_key = SigningKey::from_slice(secret_key).expect("valid secret key");
    let (sig, recovery_id) = signing_key.sign_prehash_recoverable(msg_hash).expect("sign");

    let mut result = [0u8; 65];
    let r_bytes = sig.r().to_bytes();
    let s_bytes = sig.s().to_bytes();
    result[..32].copy_from_slice(&r_bytes);
    result[32..64].copy_from_slice(&s_bytes);
    result[64] = recovery_id.to_byte();
    result
}

/// Verify a secp256k1 signature against a public key and message hash.
pub fn secp256k1_verify(
    public_key: &PublicKey,
    signature: &Signature,
    msg_hash: &[u8; 32],
) -> Result<(), Secp256k1Error> {
    let verifying_key = VerifyingKey::from_sec1_bytes(
        &[&[0x04], public_key.as_slice()].concat(),
    )
    .map_err(|_| Secp256k1Error::InvalidSignature)?;

    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|_| Secp256k1Error::InvalidSignature)?;

    verifying_key
        .verify_prehash(msg_hash, &sig)
        .map_err(|_| Secp256k1Error::VerificationFailed)
}

/// Recover the Ethereum address from a message hash and 65-byte signature.
///
/// Uses the recovery ID (v byte, index 64) to determine which of the
/// two possible public keys was used to sign.
pub fn recover_secp256k1_signer(
    msg_hash: &[u8; 32],
    signature: &Signature,
) -> Result<Address, Secp256k1Error> {
    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|_| Secp256k1Error::InvalidSignature)?;

    let recovery_id = RecoveryId::from_byte(signature[64])
        .ok_or(Secp256k1Error::RecoveryFailed)?;

    let recovered_pk = VerifyingKey::recover_from_prehash(msg_hash, &sig, recovery_id)
        .map_err(|_| Secp256k1Error::RecoveryFailed)?;

    // Ethereum address = last 20 bytes of keccak256(public_key)
    let encoded = recovered_pk.to_encoded_point(false);
    let mut hasher = Keccak256::new();
    hasher.update(&encoded.as_bytes()[1..]); // skip 0x04 prefix
    let hash = hasher.finalize();
    Ok(Address::from_slice(&hash[12..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::keccak256;

    #[test]
    fn test_secp256k1_sign_verify() {
        let (secret, pubkey) = generate_keypair();
        let msg_hash = keccak256(b"test message");
        let sig = secp256k1_sign(&secret, &msg_hash);
        assert!(secp256k1_verify(&pubkey, &sig, &msg_hash).is_ok());
    }

    #[test]
    fn test_secp256k1_recover_signer() {
        let (secret, pubkey) = generate_keypair();
        let msg_hash = keccak256(b"recover test");
        let sig = secp256k1_sign(&secret, &msg_hash);

        let addr = recover_secp256k1_signer(&msg_hash, &sig).expect("recover");
        assert_eq!(addr.as_slice().len(), 20);

        // Verify the recovered address matches the public key
        let expected_addr = pubkey_to_address(&pubkey);
        assert_eq!(addr, expected_addr);
    }

    #[test]
    fn test_secp256k1_verify_wrong_message_fails() {
        let (secret, pubkey) = generate_keypair();
        let msg_a = keccak256(b"message a");
        let sig = secp256k1_sign(&secret, &msg_a);

        let msg_b = keccak256(b"message b");
        assert!(
            secp256k1_verify(&pubkey, &sig, &msg_b).is_err(),
            "signature for different message should fail"
        );
    }

    #[test]
    fn test_secp256k1_recover_wrong_signer_fails() {
        let (secret_a, _) = generate_keypair();
        let (_, pubkey_b) = generate_keypair();
        let msg = keccak256(b"same message");
        let sig = secp256k1_sign(&secret_a, &msg);

        let recovered = recover_secp256k1_signer(&msg, &sig).expect("recover");
        let addr_b = pubkey_to_address(&pubkey_b);
        assert_ne!(recovered, addr_b, "recovered address should not match a different key");
    }

    #[test]
    fn test_secp256k1_tampered_signature_fails() {
        let (secret, pubkey) = generate_keypair();
        let msg = keccak256(b"test");
        let mut sig = secp256k1_sign(&secret, &msg);

        // Flip one byte in the signature
        sig[0] ^= 0xFF;
        assert!(
            secp256k1_verify(&pubkey, &sig, &msg).is_err(),
            "tampered signature should fail verification"
        );
    }

    /// Helper: derive address from public key bytes
    fn pubkey_to_address(pubkey: &PublicKey) -> Address {
        let mut hasher = Keccak256::new();
        hasher.update(pubkey.as_slice());
        let hash = hasher.finalize();
        Address::from_slice(&hash[12..])
    }
}

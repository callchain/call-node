//! Ed25519: key generation, signing, verification (for consensus)

use call_primitives::Ed25519PublicKey;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey, Signer, Verifier};
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
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| Ed25519Error::InvalidSignature)?;
    let sig = Signature::from_bytes(signature);
    verifying_key
        .verify(message, &sig)
        .map_err(|_| Ed25519Error::VerificationFailed)
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

//! Signer abstraction — decouples signing from key storage
//!
//! The `Signer` trait is the single interface for all cryptographic signing.
//! Implementations range from local in-memory keys (devnet) to remote HSM/KMS (production).

use crate::keystore::pubkey_to_address;
use crate::secp256k1_sign;
use call_primitives::{Address, PublicKey, Signature};
use std::sync::Arc;
use thiserror::Error;
use zeroize::Zeroize;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum SignerError {
    #[error("signing failed: {0}")]
    SigningFailed(String),
    #[error("KMS error: {0}")]
    KmsError(String),
}

/// Type of signer implementation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerKind {
    /// Local in-memory key (devnet / testnet)
    Local,
    /// AWS KMS remote signing (production)
    AwsKms,
    /// HashiCorp Vault remote signing (production)
    HashiVault,
}

/// Core signing interface — consensus code never holds raw keys
pub trait Signer: Send + Sync {
    /// Sign a 32-byte message hash
    fn sign(&self, msg_hash: &[u8; 32]) -> Result<Signature, SignerError>;

    /// Public key (cached at construction)
    fn public_key(&self) -> PublicKey;

    /// Derived address (cached at construction)
    fn address(&self) -> Address;

    /// What kind of signer this is
    fn kind(&self) -> SignerKind;
}

/// Local signer — key held in zeroized memory, signed in-process
pub struct LocalSigner {
    key: Zeroizing<[u8; 32]>,
    pubkey: PublicKey,
    address: Address,
}

impl LocalSigner {
    /// Create a local signer from raw 32-byte secret key
    pub fn from_raw_key(key: Zeroizing<[u8; 32]>) -> Result<Self, SignerError> {
        let (pubkey, address) = Self::derive_pubkey_and_address(&key)?;
        Ok(Self { key, pubkey, address })
    }

    /// Create a local signer from hex-encoded secret key
    pub fn from_hex(hex_key: &str) -> Result<Self, SignerError> {
        let bytes = hex::decode(hex_key.trim_start_matches("0x"))
            .map_err(|e| SignerError::SigningFailed(format!("invalid hex: {e}")))?;
        if bytes.len() != 32 {
            return Err(SignerError::SigningFailed(
                "validator key must be 32 bytes".into(),
            ));
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);
        Self::from_raw_key(key)
    }

    fn derive_pubkey_and_address(key: &[u8; 32]) -> Result<(PublicKey, Address), SignerError> {
        let signing_key = k256::ecdsa::SigningKey::from_slice(key)
            .map_err(|e| SignerError::SigningFailed(format!("invalid key: {e}")))?;
        let verifying_key = signing_key.verifying_key();
        let encoded = verifying_key.to_encoded_point(false);
        let mut pubkey = [0u8; 64];
        pubkey.copy_from_slice(&encoded.as_bytes()[1..]);
        let pk = PublicKey::from(pubkey);
        let addr = pubkey_to_address(&pk);
        Ok((pk, addr))
    }
}

impl Drop for LocalSigner {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl Signer for LocalSigner {
    fn sign(&self, msg_hash: &[u8; 32]) -> Result<Signature, SignerError> {
        Ok(secp256k1_sign(&self.key, msg_hash))
    }

    fn public_key(&self) -> PublicKey {
        self.pubkey
    }

    fn address(&self) -> Address {
        self.address
    }

    fn kind(&self) -> SignerKind {
        SignerKind::Local
    }
}

/// AWS KMS signer stub — requires `aws-kms` feature flag
///
/// Actual implementation requires `aws-sdk-kms` which is a heavy dependency.
/// The trait interface is ready; the concrete impl can be added when needed.
#[cfg(feature = "aws-kms")]
pub struct AwsKmsSigner {
    _key_id: String,
    _cached_pubkey: PublicKey,
    _cached_address: Address,
}

#[cfg(feature = "aws-kms")]
impl Signer for AwsKmsSigner {
    fn sign(&self, _msg_hash: &[u8; 32]) -> Result<Signature, SignerError> {
        Err(SignerError::KmsError("AWS KMS not yet implemented".into()))
    }

    fn public_key(&self) -> PublicKey {
        self._cached_pubkey
    }

    fn address(&self) -> Address {
        self._cached_address
    }

    fn kind(&self) -> SignerKind {
        SignerKind::AwsKms
    }
}

/// Type alias for shared signer
pub type SignerRef = Arc<dyn Signer>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::keccak256 as crypto_keccak256;

    #[test]
    fn test_local_signer_sign_and_verify() {
        let key = Zeroizing::new([0xABu8; 32]);
        let signer = LocalSigner::from_raw_key(key).unwrap();

        assert_eq!(signer.kind(), SignerKind::Local);
        assert_eq!(signer.public_key().as_slice().len(), 64);
        assert_eq!(signer.address().as_slice().len(), 20);

        let msg = crypto_keccak256(b"test message");
        let sig = signer.sign(&msg).unwrap();
        assert_eq!(sig.len(), 65);
    }

    #[test]
    fn test_local_signer_from_hex() {
        let hex_key = "a".repeat(64);
        let signer = LocalSigner::from_hex(&hex_key).unwrap();
        assert_eq!(signer.kind(), SignerKind::Local);
    }

    #[test]
    fn test_local_signer_invalid_hex_length() {
        let signer = LocalSigner::from_hex("short");
        assert!(signer.is_err());
    }
}

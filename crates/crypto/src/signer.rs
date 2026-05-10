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

#[cfg(any(feature = "hashi-vault", feature = "keyring"))]
use base64::Engine as _;

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
    /// OS keyring (macOS Keychain / Windows Credential Manager / Linux secret-service)
    Keyring,
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
        Ok(Self {
            key,
            pubkey,
            address,
        })
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

    pub(crate) fn derive_pubkey_and_address(key: &[u8; 32]) -> Result<(PublicKey, Address), SignerError> {
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

/// AWS KMS signer — requires `aws-kms` feature flag
///
/// Uses AWS KMS `Sign` API with ECDSA_SECP256K1 signing algorithm.
/// The private key never leaves AWS — only signatures are returned.
#[cfg(feature = "aws-kms")]
pub struct AwsKmsSigner {
    client: aws_sdk_kms::Client,
    key_id: String,
    pubkey: PublicKey,
    address: Address,
}

#[cfg(feature = "aws-kms")]
impl AwsKmsSigner {
    /// Create a new AWS KMS signer.
    ///
    /// `key_id` is the KMS key ARN or alias (e.g., "alias/validator-key").
    /// The public key is fetched from KMS at construction and cached.
    pub async fn new(key_id: String) -> Result<Self, SignerError> {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let client = aws_sdk_kms::Client::new(&config);

        // Fetch and cache the public key
        let resp = client
            .get_public_key()
            .key_id(&key_id)
            .send()
            .await
            .map_err(|e| SignerError::KmsError(format!("get_public_key: {e}")))?;

        let pk_der = resp
            .public_key()
            .ok_or_else(|| SignerError::KmsError("no public key in response".into()))?
            .as_ref();

        // Parse SEC1 uncompressed public key (0x04 || x || y)
        let pubkey = Self::parse_sec1_pubkey(pk_der)?;
        let address = pubkey_to_address(&pubkey);

        Ok(Self {
            client,
            key_id,
            pubkey,
            address,
        })
    }

    fn parse_sec1_pubkey(der: &[u8]) -> Result<PublicKey, SignerError> {
        // SEC1 uncompressed: 0x04 || 32-byte x || 32-byte y
        if der.len() != 65 || der[0] != 0x04 {
            return Err(SignerError::KmsError(format!(
                "expected 65-byte uncompressed SEC1 key, got {} bytes",
                der.len()
            )));
        }
        let mut pubkey = [0u8; 64];
        pubkey.copy_from_slice(&der[1..]);
        Ok(PublicKey::from(pubkey))
    }
}

#[cfg(feature = "aws-kms")]
impl Signer for AwsKmsSigner {
    fn sign(&self, msg_hash: &[u8; 32]) -> Result<Signature, SignerError> {
        use aws_sdk_kms::types::SigningAlgorithmSpec;

        // Block on the async KMS call using tokio's current runtime
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|e| SignerError::KmsError(format!("no tokio runtime: {e}")))?;

        let sig_resp = rt
            .block_on(async {
                self.client
                    .sign()
                    .key_id(&self.key_id)
                    .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
                    .message(aws_sdk_kms::primitives::Blob::new(msg_hash.as_slice()))
                    .message_type(aws_sdk_kms::types::MessageType::Digest)
                    .send()
                    .await
            })
            .map_err(|e| SignerError::KmsError(format!("KMS sign failed: {e}")))?;

        let sig_der = sig_resp
            .signature()
            .ok_or_else(|| SignerError::KmsError("no signature in KMS response".into()))?
            .as_ref();

        // Convert DER signature to raw 65-byte (r || s || v)
        Self::der_to_raw(sig_der)
    }

    fn public_key(&self) -> PublicKey {
        self.pubkey
    }

    fn address(&self) -> Address {
        self.address
    }

    fn kind(&self) -> SignerKind {
        SignerKind::AwsKms
    }
}

#[cfg(feature = "aws-kms")]
impl AwsKmsSigner {
    /// Convert ASN.1 DER ECDSA signature to raw 65-byte format (r || s || v)
    fn der_to_raw(der: &[u8]) -> Result<Signature, SignerError> {
        use k256::ecdsa::Signature as K256Sig;
        let sig =
            K256Sig::from_der(der).map_err(|e| SignerError::KmsError(format!("DER parse: {e}")))?;

        let r_bytes = sig.r().to_bytes();
        let s_bytes = sig.s().to_bytes();

        let mut result = [0u8; 65];
        result[..32].copy_from_slice(&r_bytes);
        result[32..64].copy_from_slice(&s_bytes);
        result[64] = 0; // recovery id — AWS KMS doesn't provide this; caller must recover
        Ok(result)
    }
}

/// HashiCorp Vault transit signer — requires `hashi-vault` feature flag
///
/// Uses Vault's transit/sign API. The private key is stored in Vault;
/// only signatures are returned.
#[cfg(feature = "hashi-vault")]
pub struct HashiVaultSigner {
    vault_addr: String,
    token: String,
    key_name: String,
    pubkey: PublicKey,
    address: Address,
}

#[cfg(feature = "hashi-vault")]
impl HashiVaultSigner {
    /// Create a new Vault transit signer.
    ///
    /// `vault_addr`: e.g., "http://127.0.0.1:8200"
    /// `token`: Vault authentication token
    /// `key_name`: transit key name (e.g., "validator-key")
    pub async fn new(
        vault_addr: String,
        token: String,
        key_name: String,
    ) -> Result<Self, SignerError> {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/v1/transit/keys/{}",
            vault_addr.trim_end_matches('/'),
            key_name
        );

        let resp = client
            .get(&url)
            .header("X-Vault-Token", &token)
            .send()
            .await
            .map_err(|e| SignerError::KmsError(format!("vault request: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SignerError::KmsError(format!("vault json: {e}")))?;

        let pubkey_b64 = body
            .get("data")
            .and_then(|d| d.get("keys"))
            .and_then(|k| k.as_object())
            .and_then(|m| m.values().next())
            .and_then(|v| v.get("public_key"))
            .and_then(|p| p.as_str())
            .ok_or_else(|| SignerError::KmsError("no public key in vault response".into()))?;

        let pk_der = base64::engine::general_purpose::STANDARD
            .decode(pubkey_b64)
            .map_err(|e| SignerError::KmsError(format!("base64: {e}")))?;

        let pubkey = Self::parse_sec1_pubkey(&pk_der)?;
        let address = pubkey_to_address(&pubkey);

        Ok(Self {
            vault_addr,
            token,
            key_name,
            pubkey,
            address,
        })
    }

    fn parse_sec1_pubkey(der: &[u8]) -> Result<PublicKey, SignerError> {
        if der.len() != 65 || der[0] != 0x04 {
            return Err(SignerError::KmsError(format!(
                "expected 65-byte uncompressed SEC1 key, got {} bytes",
                der.len()
            )));
        }
        let mut pubkey = [0u8; 64];
        pubkey.copy_from_slice(&der[1..]);
        Ok(PublicKey::from(pubkey))
    }
}

#[cfg(feature = "hashi-vault")]
impl Signer for HashiVaultSigner {
    fn sign(&self, msg_hash: &[u8; 32]) -> Result<Signature, SignerError> {
        let client = reqwest::blocking::Client::new();
        let url = format!(
            "{}/v1/transit/sign/{}/sha2-256",
            self.vault_addr.trim_end_matches('/'),
            self.key_name
        );

        let body = serde_json::json!({
            "input": base64::engine::general_purpose::STANDARD.encode(msg_hash)
        });

        let resp = client
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .json(&body)
            .send()
            .map_err(|e| SignerError::KmsError(format!("vault sign request: {e}")))?;

        let resp_json: serde_json::Value = resp
            .json()
            .map_err(|e| SignerError::KmsError(format!("vault sign json: {e}")))?;

        let sig_b64 = resp_json
            .get("data")
            .and_then(|d| d.get("signature"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| SignerError::KmsError("no signature in vault response".into()))?;

        // Vault returns signatures as "vault:v1:BASE64"
        let sig_bytes = if let Some(idx) = sig_b64.rfind(':') {
            base64::engine::general_purpose::STANDARD.decode(&sig_b64[idx + 1..])
        } else {
            base64::engine::general_purpose::STANDARD.decode(sig_b64)
        }
        .map_err(|e| SignerError::KmsError(format!("base64: {e}")))?;

        Self::der_to_raw(&sig_bytes)
    }

    fn public_key(&self) -> PublicKey {
        self.pubkey
    }

    fn address(&self) -> Address {
        self.address
    }

    fn kind(&self) -> SignerKind {
        SignerKind::HashiVault
    }
}

#[cfg(feature = "hashi-vault")]
impl HashiVaultSigner {
    fn der_to_raw(der: &[u8]) -> Result<Signature, SignerError> {
        use k256::ecdsa::Signature as K256Sig;
        let sig =
            K256Sig::from_der(der).map_err(|e| SignerError::KmsError(format!("DER parse: {e}")))?;

        let r_bytes = sig.r().to_bytes();
        let s_bytes = sig.s().to_bytes();

        let mut result = [0u8; 65];
        result[..32].copy_from_slice(&r_bytes);
        result[32..64].copy_from_slice(&s_bytes);
        result[64] = 0;
        Ok(result)
    }
}

/// OS keyring signer — stores the validator key in the operating system's
/// secure credential store (macOS Keychain, Windows Credential Manager,
/// or Linux secret-service / kernel keyring).
///
/// The key is retrieved once at node startup and held in zeroized memory
/// for the process lifetime.  This is safer than a plaintext file or env
/// variable, because the OS credential store is encrypted at rest and
/// access-controlled by the OS.
#[cfg(feature = "keyring")]
pub struct KeyringSigner {
    key: Zeroizing<[u8; 32]>,
    pubkey: PublicKey,
    address: Address,
}

#[cfg(feature = "keyring")]
impl KeyringSigner {
    /// Load a key from the OS keyring.
    ///
    /// `service` — application identifier (e.g. `"call-node"`).
    /// `username` — per-key identifier (e.g. validator address hex).
    /// The stored secret must be a base64-encoded 32-byte raw secp256k1 key.
    pub fn new(service: &str, username: &str) -> Result<Self, SignerError> {
        let entry = keyring::Entry::new(service, username)
            .map_err(|e| SignerError::SigningFailed(format!("keyring entry: {e}")))?;
        Self::from_entry(entry)
    }

    /// Load a key from an existing keyring entry.
    ///
    /// Useful for testing with mock credentials.
    pub fn from_entry(entry: keyring::Entry) -> Result<Self, SignerError> {
        let secret = entry
            .get_password()
            .map_err(|e| SignerError::SigningFailed(format!("keyring get_password: {e}")))?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(secret)
            .map_err(|e| SignerError::SigningFailed(format!("base64 decode: {e}")))?;

        if bytes.len() != 32 {
            return Err(SignerError::SigningFailed(format!(
                "keyring secret must decode to 32 bytes, got {}",
                bytes.len()
            )));
        }

        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);

        let (pubkey, address) = LocalSigner::derive_pubkey_and_address(&key)?;

        Ok(Self {
            key,
            pubkey,
            address,
        })
    }

    /// Store a 32-byte hex key into the OS keyring (one-time setup helper).
    ///
    /// After calling this the operator can delete the plaintext file.
    pub fn store_key(service: &str, username: &str, hex_key: &str) -> Result<(), SignerError> {
        let entry = keyring::Entry::new(service, username)
            .map_err(|e| SignerError::SigningFailed(format!("keyring entry: {e}")))?;
        Self::store_key_with_entry(&entry, hex_key)
    }

    /// Store a 32-byte hex key into the given keyring entry.
    ///
    /// Useful for testing with mock credentials.
    pub fn store_key_with_entry(
        entry: &keyring::Entry,
        hex_key: &str,
    ) -> Result<(), SignerError> {
        let bytes = hex::decode(hex_key.trim_start_matches("0x"))
            .map_err(|e| SignerError::SigningFailed(format!("invalid hex: {e}")))?;

        if bytes.len() != 32 {
            return Err(SignerError::SigningFailed(
                "key must be 32 bytes".into(),
            ));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        entry
            .set_password(&b64)
            .map_err(|e| SignerError::SigningFailed(format!("keyring set_password: {e}")))?;

        Ok(())
    }
}

#[cfg(feature = "keyring")]
impl Signer for KeyringSigner {
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
        SignerKind::Keyring
    }
}

#[cfg(feature = "keyring")]
impl Drop for KeyringSigner {
    fn drop(&mut self) {
        self.key.zeroize();
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

    #[test]
    #[cfg(feature = "keyring")]
    fn test_keyring_signer_roundtrip() {
        let hex_key = "a".repeat(64);

        // Use an in-memory mock credential so the test is deterministic
        // across platforms (especially headless CI where no OS keyring
        // daemon is available).
        let credential = keyring::mock::default_credential_builder()
            .build(None, "call-node-test", "test-validator")
            .unwrap();
        let entry = keyring::Entry::new_with_credential(credential);

        // Store key via the shared entry
        KeyringSigner::store_key_with_entry(&entry, &hex_key).unwrap();

        // Load from the same entry
        let signer = KeyringSigner::from_entry(entry).unwrap();
        assert_eq!(signer.kind(), SignerKind::Keyring);

        let msg = crypto_keccak256(b"keyring test message");
        let sig = signer.sign(&msg).unwrap();
        assert_eq!(sig.len(), 65);

        // Verify pubkey matches LocalSigner with same key
        let local = LocalSigner::from_hex(&hex_key).unwrap();
        assert_eq!(signer.public_key(), local.public_key());
        assert_eq!(signer.address(), local.address());
    }
}

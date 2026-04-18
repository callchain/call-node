//! Encrypted keystore — Web3 Secret Storage compatible
//!
//! KDF: scrypt (N=262144, r=8, p=1)
//! Cipher: AES-128-CTR
//! MAC: keccak256(derived_key[16..32] || ciphertext)

use crate::hash::keccak256;
use call_primitives::PublicKey;
use k256::ecdsa::SigningKey;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::path::Path;
use thiserror::Error;
use zeroize::Zeroizing;

const SCRYPT_N: u32 = 262_144;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

#[derive(Debug, Error)]
pub enum KeystoreError {
    #[error("incorrect passphrase")]
    IncorrectPassphrase,
    #[error("invalid keystore data")]
    InvalidData,
    #[error("IO error: {0}")]
    Io(String),
    #[error("crypto error: {0}")]
    Crypto(String),
}

/// Web3 Secret Storage-compatible encrypted keystore
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKeystore {
    pub ciphertext: Vec<u8>,
    pub salt: [u8; 32],
    pub iv: [u8; 16],
    pub mac: [u8; 32],
}

/// Derive encryption key from passphrase using scrypt
fn derive_key(passphrase: &str, salt: &[u8; 32]) -> Result<[u8; 32], KeystoreError> {
    let mut dk = [0u8; 32];
    scrypt::scrypt(
        passphrase.as_bytes(),
        salt,
        &scrypt::Params::new(
            log2_param(SCRYPT_N),
            SCRYPT_R,
            SCRYPT_P,
            scrypt::Params::RECOMMENDED_LEN,
        )
        .map_err(|e| KeystoreError::Crypto(format!("scrypt params: {e}")))?,
        &mut dk,
    )
    .map_err(|e| KeystoreError::Crypto(format!("scrypt derive: {e}")))?;
    Ok(dk)
}

fn log2_param(n: u32) -> u8 {
    32 - n.leading_zeros() as u8 - 1
}

/// Encrypt a raw private key to an encrypted keystore
pub fn encrypt_key(
    raw_key: &[u8; 32],
    passphrase: &str,
) -> Result<EncryptedKeystore, KeystoreError> {
    use aes::cipher::{KeyIvInit, StreamCipher};
    type Aes128Ctr64BE = ctr::Ctr64BE<aes::Aes128>;

    let mut salt = [0u8; 32];
    OsRng.fill_bytes(&mut salt);

    let mut iv = [0u8; 16];
    OsRng.fill_bytes(&mut iv);

    let dk = derive_key(passphrase, &salt)?;
    let enc_key = &dk[..16];
    let mac_key = &dk[16..];

    let mut ciphertext = raw_key.to_vec();
    let mut cipher = Aes128Ctr64BE::new(enc_key.into(), &iv.into());
    cipher.apply_keystream(&mut ciphertext);

    // MAC = keccak256(mac_key || ciphertext)
    let mut mac_input = Vec::with_capacity(mac_key.len() + ciphertext.len());
    mac_input.extend_from_slice(mac_key);
    mac_input.extend_from_slice(&ciphertext);
    let mac_hash = keccak256(&mac_input);
    let mut mac = [0u8; 32];
    mac.copy_from_slice(mac_hash.as_slice());

    Ok(EncryptedKeystore {
        ciphertext,
        salt,
        iv,
        mac,
    })
}

/// Decrypt an encrypted keystore, returning the raw key (zeroized on drop)
pub fn decrypt_key(
    keystore: &EncryptedKeystore,
    passphrase: &str,
) -> Result<Zeroizing<[u8; 32]>, KeystoreError> {
    use aes::cipher::{KeyIvInit, StreamCipher};
    type Aes128Ctr64BE = ctr::Ctr64BE<aes::Aes128>;

    let dk = derive_key(passphrase, &keystore.salt)?;
    let enc_key = &dk[..16];
    let mac_key = &dk[16..];

    // Verify MAC
    let mut mac_input = Vec::with_capacity(mac_key.len() + keystore.ciphertext.len());
    mac_input.extend_from_slice(mac_key);
    mac_input.extend_from_slice(&keystore.ciphertext);
    let computed_mac = keccak256(&mac_input);
    if computed_mac != keystore.mac {
        return Err(KeystoreError::IncorrectPassphrase);
    }

    let mut plaintext = keystore.ciphertext.clone();
    let mut cipher = Aes128Ctr64BE::new(enc_key.into(), &keystore.iv.into());
    cipher.apply_keystream(&mut plaintext);

    if plaintext.len() != 32 {
        return Err(KeystoreError::InvalidData);
    }

    let mut result = Zeroizing::new([0u8; 32]);
    result.copy_from_slice(&plaintext);
    Ok(result)
}

/// Generate a new secp256k1 keypair, encrypt, and write to file
pub fn generate_and_store(
    path: &Path,
    passphrase: &str,
) -> Result<PublicKey, KeystoreError> {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let encoded = verifying_key.to_encoded_point(false);
    let mut pubkey = [0u8; 64];
    pubkey.copy_from_slice(&encoded.as_bytes()[1..]);

    let raw_key: [u8; 32] = *signing_key.to_bytes().as_ref();
    let keystore = encrypt_key(&raw_key, passphrase)?;

    let json = serde_json::to_string_pretty(&keystore)
        .map_err(|e| KeystoreError::Io(format!("serialize: {e}")))?;
    std::fs::write(path, json)
        .map_err(|e| KeystoreError::Io(format!("write: {e}")))?;

    Ok(PublicKey::from(pubkey))
}

/// Read and decrypt key from file
pub fn load_key(
    path: &Path,
    passphrase: &str,
) -> Result<Zeroizing<[u8; 32]>, KeystoreError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| KeystoreError::Io(format!("read: {e}")))?;
    let keystore: EncryptedKeystore = serde_json::from_str(&content)
        .map_err(|_e| KeystoreError::InvalidData)?;
    decrypt_key(&keystore, passphrase)
}

/// Derive address from public key (keccak256, last 20 bytes)
pub fn pubkey_to_address(pubkey: &PublicKey) -> call_primitives::Address {
    let mut hasher = Keccak256::new();
    hasher.update(pubkey.as_slice());
    let hash = hasher.finalize();
    call_primitives::Address::from_slice(&hash[12..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = SigningKey::random(&mut OsRng);
        let raw: [u8; 32] = *key.to_bytes().as_ref();
        let keystore = encrypt_key(&raw, "test-password").unwrap();
        let decrypted = decrypt_key(&keystore, "test-password").unwrap();
        assert_eq!(raw.as_slice(), decrypted.as_slice());
    }

    #[test]
    fn test_wrong_passphrase() {
        let key = SigningKey::random(&mut OsRng);
        let raw: [u8; 32] = *key.to_bytes().as_ref();
        let keystore = encrypt_key(&raw, "correct").unwrap();
        assert!(matches!(
            decrypt_key(&keystore, "wrong"),
            Err(KeystoreError::IncorrectPassphrase)
        ));
    }

    #[test]
    fn test_generate_and_store() {
        let tmp = std::env::temp_dir().join("test_keystore.json");
        let pubkey = generate_and_store(&tmp, "pass123").unwrap();
        assert!(pubkey.as_slice().len() == 64);
        assert!(tmp.exists());

        let loaded = load_key(&tmp, "pass123").unwrap();
        let signing = SigningKey::from_slice(&*loaded).unwrap();
        assert_eq!(signing.verifying_key().to_encoded_point(false).as_bytes()[1..].to_vec(), pubkey.as_slice().to_vec());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_pubkey_to_address() {
        let key = SigningKey::random(&mut OsRng);
        let verifying = key.verifying_key();
        let encoded = verifying.to_encoded_point(false);
        let mut pubkey = [0u8; 64];
        pubkey.copy_from_slice(&encoded.as_bytes()[1..]);
        let pk = PublicKey::from(pubkey);

        let addr = pubkey_to_address(&pk);
        assert_eq!(addr.as_slice().len(), 20);
    }
}

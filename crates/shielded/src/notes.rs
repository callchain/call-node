//! Shielded notes (per spec §3.8.1)
//!
//! Note-based UTXO model with encrypted values and commitment derivation.

use call_primitives::{AssetId, Balance, Hash};
use crate::{NoteCommitment, Nullifier, ViewingKey, poseidon};

/// A shielded note: encrypted value with commitment and nullifier derivation
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Note {
    pub value: Balance,
    pub asset_id: AssetId,
    /// Random commitment mask (rcm)
    rcm: [u8; 32],
    /// Recipient's incoming viewing key
    recipient_ivk: [u8; 32],
    /// Rho: unique identifier for nullifier derivation
    rho: [u8; 32],
}

impl Note {
    /// Create a new shielded note.
    ///
    /// RCM is derived via Poseidon: H("rcm" || ivk || value || asset || rho).
    pub fn new(
        value: Balance,
        asset_id: AssetId,
        viewing_key: &ViewingKey,
        rho: Hash,
    ) -> Self {
        let ivk_fr = poseidon::bytes_to_fr(&viewing_key.incoming_view_key);
        let value_bytes = poseidon::value_to_fr_bytes(value);
        let value_fr = poseidon::bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = poseidon::bytes_to_fr(&asset_bytes);
        let rho_fr = poseidon::bytes_to_fr(&rho.0);
        let rcm_fr = poseidon::poseidon_hash_tagged(poseidon::domain::RCM, &[ivk_fr, value_fr, asset_fr, rho_fr]);
        let rcm = poseidon::fr_to_bytes(&rcm_fr);

        Self {
            value,
            asset_id,
            rcm,
            recipient_ivk: viewing_key.incoming_view_key,
            rho: rho.0,
        }
    }

    /// Compute the note commitment (what goes into the Merkle tree).
    ///
    /// Matches the R1CS circuit: commitment = H(value || asset || rcm || rho).
    pub fn commitment(&self) -> NoteCommitment {
        let value_bytes = poseidon::value_to_fr_bytes(self.value);
        let value_fr = poseidon::bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&self.asset_id.to_le_bytes());
        let asset_fr = poseidon::bytes_to_fr(&asset_bytes);
        let rcm_fr = poseidon::bytes_to_fr(&self.rcm);
        let rho_fr = poseidon::bytes_to_fr(&self.rho);
        let cm_fr = poseidon::poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        NoteCommitment::new(Hash::from_slice(&poseidon::fr_to_bytes(&cm_fr)))
    }

    /// Derive the nullifier for this note (what marks it as spent).
    ///
    /// Matches the R1CS circuit: fvk = H("fvk_from_ivk" || ivk), nullifier = H(fvk || rho).
    pub fn nullifier(&self) -> Nullifier {
        let ivk_fr = poseidon::bytes_to_fr(&self.recipient_ivk);
        let fvk_fr = poseidon::poseidon_hash_tagged(poseidon::domain::FVK_FROM_IVK, &[ivk_fr]);
        let fvk = poseidon::fr_to_bytes(&fvk_fr);
        let vk = ViewingKey::from_incoming_view_key(self.recipient_ivk, fvk);
        vk.derive_nullifier(&self.rho)
    }

    /// Get the RCM (for viewing key verification)
    pub fn rcm(&self) -> &[u8; 32] {
        &self.rcm
    }

    /// Get the rho (unique identifier)
    pub fn rho(&self) -> &[u8; 32] {
        &self.rho
    }

    /// Get the asset ID
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    /// Serialize the note for encrypted storage
    pub fn to_encrypted_bytes(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&self.value.to_le_bytes());
        data.extend_from_slice(&self.asset_id.to_le_bytes());
        data.extend_from_slice(&self.rcm);
        data.extend_from_slice(&self.rho);
        data.extend_from_slice(&self.recipient_ivk);
        data
    }

    /// Deserialize from encrypted bytes
    pub fn from_encrypted_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 8 + 8 + 32 + 32 + 32 {
            return Err("note data too short");
        }
        let value = u128::from_le_bytes(data[0..16].try_into().map_err(|_| "invalid value")?);
        let asset_id = u64::from_le_bytes(data[16..24].try_into().map_err(|_| "invalid asset_id")?);
        let mut rcm = [0u8; 32];
        rcm.copy_from_slice(&data[24..56]);
        let mut rho = [0u8; 32];
        rho.copy_from_slice(&data[56..88]);
        let mut recipient_ivk = [0u8; 32];
        recipient_ivk.copy_from_slice(&data[88..120]);

        Ok(Self {
            value,
            asset_id,
            rcm,
            recipient_ivk,
            rho,
        })
    }
}

/// Note encryption/decryption using ChaCha20-Poly1305
pub mod encryption {
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, aead::{Aead, KeyInit}};
    use rand::RngCore;

    /// Encrypt note data with a symmetric key derived from viewing key
    pub fn encrypt_note(plaintext: &[u8], ivk: &[u8; 32]) -> Vec<u8> {
        let key = Key::from_slice(ivk);
        let cipher = ChaCha20Poly1305::new(key);

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, plaintext)
            .expect("encryption should not fail");

        // Prepend nonce to ciphertext
        let mut output = Vec::with_capacity(12 + ciphertext.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);
        output
    }

    /// Decrypt note data with the viewing key
    pub fn decrypt_note(ciphertext: &[u8], ivk: &[u8; 32]) -> Result<Vec<u8>, &'static str> {
        if ciphertext.len() < 12 {
            return Err("ciphertext too short");
        }

        let key = Key::from_slice(ivk);
        let cipher = ChaCha20Poly1305::new(key);

        let nonce_bytes = &ciphertext[..12];
        let payload = &ciphertext[12..];
        let nonce = Nonce::from_slice(nonce_bytes);

        cipher.decrypt(nonce, payload)
            .map_err(|_| "decryption failed")
    }

    /// Try to decrypt note data, returning false on any failure.
    /// Used by ViewingKey::can_decrypt for actual decryption attempts.
    pub fn try_decrypt_note(ciphertext: &[u8], ivk: &[u8; 32]) -> bool {
        decrypt_note(ciphertext, ivk).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{test_hash, test_spending_key};
    use crate::ViewingKey;

    #[test]
    fn test_note_creation() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note = Note::new(1000, 1, &vk, test_hash(7));

        assert_eq!(note.value, 1000);
        assert_eq!(note.asset_id(), 1);
        assert_eq!(note.rcm().len(), 32);
        assert_eq!(note.rho().len(), 32);
    }

    #[test]
    fn test_note_commitment_is_deterministic() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note = Note::new(1000, 1, &vk, test_hash(7));

        let cm1 = note.commitment();
        let cm2 = note.commitment();
        assert_eq!(cm1, cm2);
    }

    #[test]
    fn test_note_nullifier_is_deterministic() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note = Note::new(1000, 1, &vk, test_hash(7));

        let nf1 = note.nullifier();
        let nf2 = note.nullifier();
        assert_eq!(nf1, nf2);
    }

    #[test]
    fn test_note_different_values_different_commitments() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note1 = Note::new(1000, 1, &vk, test_hash(7));
        let note2 = Note::new(2000, 1, &vk, test_hash(7));

        assert_ne!(note1.commitment(), note2.commitment());
    }

    #[test]
    fn test_note_encryption_decryption() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note = Note::new(1000, 1, &vk, test_hash(7));

        let plaintext = note.to_encrypted_bytes();
        let encrypted = encryption::encrypt_note(&plaintext, &vk.incoming_view_key);
        let decrypted = encryption::decrypt_note(&encrypted, &vk.incoming_view_key).unwrap();
        let note2 = Note::from_encrypted_bytes(&decrypted).unwrap();

        assert_eq!(note.value, note2.value);
        assert_eq!(note.asset_id(), note2.asset_id());
        assert_eq!(note.commitment(), note2.commitment());
    }

    #[test]
    fn test_note_decryption_wrong_key() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let note = Note::new(1000, 1, &vk, test_hash(7));
        let plaintext = note.to_encrypted_bytes();
        let encrypted = encryption::encrypt_note(&plaintext, &vk.incoming_view_key);

        let wrong_ivk = ViewingKey::generate(&test_spending_key(99)).incoming_view_key;
        assert!(encryption::decrypt_note(&encrypted, &wrong_ivk).is_err());
    }
}

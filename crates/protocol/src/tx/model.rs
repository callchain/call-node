//! Transaction model, authentication, gas config, and signature serde helpers.

use call_primitives::{Address, FeeCurrency};
use crate::instructions::Instruction;

/// Serialization helper for [u8; 65] signatures
mod sig_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(sig: &[u8; 65], serializer: S) -> Result<S::Ok, S::Error> {
        sig.as_slice().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 65], D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        bytes.try_into().map_err(|_| serde::de::Error::custom("expected 65 bytes"))
    }
}

/// Serialization helper for Vec<[u8; 65]>
mod sig_vec_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(sigs: &[[u8; 65]], serializer: S) -> Result<S::Ok, S::Error> {
        // Flatten Vec<[u8; 65]> into Vec<u8> (65 bytes per signature)
        let flat: Vec<u8> = sigs.iter().flat_map(|s| s.iter().copied()).collect();
        serializer.serialize_bytes(&flat)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<[u8; 65]>, D::Error> {
        let flat = Vec::<u8>::deserialize(deserializer)?;
        if flat.len() % 65 != 0 {
            return Err(serde::de::Error::custom("signature data not multiple of 65"));
        }
        let count = flat.len() / 65;
        let mut sigs = Vec::with_capacity(count);
        for chunk in flat.chunks_exact(65) {
            sigs.push(chunk.try_into().map_err(|_| serde::de::Error::custom("chunk size mismatch"))?);
        }
        Ok(sigs)
    }
}

/// Authentication scheme (defined here, used by smart_accounts too)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AuthScheme {
    SingleSig {
        #[serde(with = "sig_serde")]
        signature: [u8; 65],
    },
    MultiSig {
        #[serde(with = "sig_vec_serde")]
        signatures: Vec<[u8; 65]>,
    },
    SessionKey {
        key: Address,
        #[serde(with = "sig_serde")]
        signature: [u8; 65],
    },
}

/// Gas payment configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum GasConfig {
    SelfPay,
    AuthorizedSponsor { sponsor: Address },
    PoolSponsor { sponsor: Address },
    PerTxSponsor { sponsor: Address, sponsor_signature: Vec<u8> },
}

/// A protocol transaction (per spec §3.5)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProtocolTransaction {
    pub sender: Address,
    pub nonce: u64,
    pub instructions: Vec<Instruction>,
    pub gas_config: GasConfig,
    pub fee_currency: FeeCurrency,
    pub gas_limit: u64,
    pub max_fee: u128,
    /// Maximum priority fee per gas unit the user is willing to pay (wei)
    #[serde(default)]
    pub max_priority_fee: u128,
    /// Block height at which this transaction expires (0 = never)
    #[serde(default)]
    pub expires_at: u64,
    pub auth: AuthScheme,
}

impl ProtocolTransaction {
    /// Compute the canonical transaction hash for signature verification.
    ///
    /// The hash covers all fields except `auth` (the signature itself),
    /// preventing signature malleability attacks.
    pub fn compute_tx_hash(&self) -> [u8; 32] {
        use call_crypto::keccak256;
        let mut preimage = Vec::new();
        preimage.extend_from_slice(self.sender.as_slice());
        preimage.extend_from_slice(&self.nonce.to_be_bytes());
        let instr_bytes = serde_json::to_vec(&self.instructions).unwrap_or_default();
        preimage.extend_from_slice(&instr_bytes);
        match &self.gas_config {
            GasConfig::SelfPay => {
                preimage.push(0);
            }
            GasConfig::AuthorizedSponsor { sponsor } => {
                preimage.push(1);
                preimage.extend_from_slice(sponsor.as_slice());
            }
            GasConfig::PoolSponsor { sponsor } => {
                preimage.push(2);
                preimage.extend_from_slice(sponsor.as_slice());
            }
            GasConfig::PerTxSponsor { sponsor, sponsor_signature } => {
                preimage.push(3);
                preimage.extend_from_slice(sponsor.as_slice());
                preimage.extend_from_slice(sponsor_signature);
            }
        }
        let fee_currency_bytes: Vec<u8> = match self.fee_currency {
            call_primitives::FeeCurrency::Call => vec![0],
            call_primitives::FeeCurrency::Stablecoin(id) => {
                let mut v = vec![1];
                v.extend_from_slice(&id.to_be_bytes());
                v
            }
        };
        preimage.extend_from_slice(&fee_currency_bytes);
        preimage.extend_from_slice(&self.gas_limit.to_be_bytes());
        preimage.extend_from_slice(&self.max_fee.to_be_bytes());
        preimage.extend_from_slice(&self.max_priority_fee.to_be_bytes());
        preimage.extend_from_slice(&self.expires_at.to_be_bytes());
        let h = keccak256(&preimage);
        h.0
    }

    /// Verify the transaction's secp256k1 signature(s).
    ///
    /// - `SingleSig`: recovers signer and checks it matches `self.sender`
    /// - `MultiSig`: recovers each signer and checks against threshold (from
    ///   `registry` if provided, otherwise defaults to 2)
    /// - `SessionKey`: recovers signer and checks it matches the session key
    pub fn verify_signature(&self) -> Result<(), crate::ProtocolError> {
        self.verify_signature_with_registry(None)
    }

    /// Verify signature with optional smart-account registry for MultiSig threshold.
    pub fn verify_signature_with_registry(
        &self,
        registry: Option<&crate::smart_accounts::SmartAccountRegistry>,
    ) -> Result<(), crate::ProtocolError> {
        use call_crypto::recover_secp256k1_signer;
        let tx_hash = self.compute_tx_hash();
        match &self.auth {
            AuthScheme::SingleSig { signature } => {
                let recovered = recover_secp256k1_signer(&tx_hash, signature)
                    .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                if recovered != self.sender {
                    return Err(crate::ProtocolError::InvalidSignature(
                        "signature does not match sender".into(),
                    ));
                }
                Ok(())
            }
            AuthScheme::MultiSig { signatures } => {
                let mut unique_signers = std::collections::HashSet::new();
                for sig in signatures {
                    let recovered = recover_secp256k1_signer(&tx_hash, sig)
                        .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                    unique_signers.insert(recovered);
                }
                let threshold = registry
                    .and_then(|r| r.get_multisig_config(&self.sender))
                    .map(|c| c.threshold as usize)
                    .unwrap_or(2);
                if unique_signers.len() < threshold {
                    return Err(crate::ProtocolError::InvalidSignature(format!(
                        "multisig requires at least {threshold} unique signers"
                    )));
                }
                Ok(())
            }
            AuthScheme::SessionKey { key, signature } => {
                let recovered = recover_secp256k1_signer(&tx_hash, signature)
                    .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                if recovered != *key {
                    return Err(crate::ProtocolError::InvalidSignature(
                        "signature does not match session key".into(),
                    ));
                }
                Ok(())
            }
        }
    }
}

//! Oracle cryptographic helpers

use crate::PricePair;
use call_crypto::ed25519_sign;
use ed25519_dalek::SigningKey;

/// Create a canonical message hash for oracle submissions
/// Protocol domain separator to prevent cross-protocol replay attacks.
const ORACLE_DOMAIN_SEPARATOR: &[u8] = b"CALL_ORACLE_V1";

pub fn oracle_message_hash(
    validator_id: u32,
    pair: PricePair,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(14 + 4 + 16 + 16 + 8 + 8);
    msg.extend_from_slice(ORACLE_DOMAIN_SEPARATOR);
    msg.extend_from_slice(&validator_id.to_le_bytes());
    msg.extend_from_slice(&pair.base.to_le_bytes());
    msg.extend_from_slice(&pair.quote.to_le_bytes());
    msg.extend_from_slice(&price.to_le_bytes());
    msg.extend_from_slice(&block_number.to_le_bytes());
    msg.extend_from_slice(&timestamp.to_le_bytes());
    msg
}

/// Sign an oracle submission message
pub fn sign_oracle_submission(
    signing_key: &SigningKey,
    validator_id: u32,
    pair: PricePair,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> [u8; 64] {
    let message = oracle_message_hash(validator_id, pair, price, block_number, timestamp);
    ed25519_sign(signing_key, &message)
}

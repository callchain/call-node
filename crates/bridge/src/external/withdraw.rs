//! External bridge withdrawal and signing logic.

use alloy_primitives::{Address, B256};
use call_primitives::{AssetId, Signature};
use call_crypto::secp256k1_sign;
use crate::external::types::{ExternalChain, bridge_event_hash};

/// Sign a bridge event as a validator (per spec §5.6.2)
///
/// Returns the secp256k1 signature (65 bytes: r || s || v) over the bridge event hash.
pub fn sign_bridge_event(
    secret_key: &[u8; 32],
    source_chain: &ExternalChain,
    source_tx_hash: B256,
    source_block_number: u64,
    sender: &[u8],
    recipient: Address,
    asset_id: AssetId,
    amount: u128,
) -> Signature {
    let event_hash = bridge_event_hash(
        source_chain,
        source_tx_hash,
        source_block_number,
        sender,
        recipient,
        asset_id,
        amount,
    );

    secp256k1_sign(secret_key, &event_hash.0)
}

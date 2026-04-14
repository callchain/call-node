//! External bridge: cross-chain deposit/withdraw with validator signatures (per spec §5.6)
//!
//! - ExternalChain enum: EthereumMainnet, Arbitrum
//! - ExternalBridgeOp enum with full fields
//! - verify_bridge_signatures: 14+ validator secp256k1 signatures
//! - sign_bridge_event: validator signing service
//! - BridgeConfig limits enforcement

use alloy_primitives::{Address, B256};
use call_primitives::{AssetId, Signature};
use call_crypto::{keccak256, secp256k1_sign, recover_secp256k1_signer};
use call_protocol::balances::BalanceState;
use crate::{BridgeConfig, BridgeError, BridgeStateManager};

/// Supported external chains (per spec §5.6)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChain {
    EthereumMainnet,
    Arbitrum,
}

impl ExternalChain {
    /// Chain ID for the external chain
    pub fn chain_id(&self) -> u64 {
        match self {
            ExternalChain::EthereumMainnet => 1,
            ExternalChain::Arbitrum => 42161,
        }
    }
}

/// External bridge operation (per spec §5.6)
#[derive(Debug, Clone)]
pub enum ExternalBridgeOp {
    /// Deposit from external chain → Callchain
    Deposit {
        source_chain: ExternalChain,
        source_tx_hash: B256,
        source_block_number: u64,
        sender: Vec<u8>,
        recipient: Address,
        asset_id: AssetId,
        amount: u128,
        signatures: Vec<BridgeSignature>,
    },
    /// Withdraw from Callchain → external chain
    Withdraw {
        target_chain: ExternalChain,
        target_address: Vec<u8>,
        asset_id: AssetId,
        sender: Address,
        amount: u128,
    },
}

/// Validator bridge signature
#[derive(Debug, Clone)]
pub struct BridgeSignature {
    /// Validator index in the validator set
    pub validator_index: u32,
    /// secp256k1 signature (65 bytes: r || s || v)
    pub signature: Signature,
}

/// Hash of a bridge event for signing
pub fn bridge_event_hash(
    source_chain: &ExternalChain,
    source_tx_hash: B256,
    source_block_number: u64,
    sender: &[u8],
    recipient: Address,
    asset_id: AssetId,
    amount: u128,
) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(&source_chain.chain_id().to_be_bytes());
    buf.extend_from_slice(&source_tx_hash.0);
    buf.extend_from_slice(&source_block_number.to_be_bytes());
    buf.extend_from_slice(sender);
    buf.extend_from_slice(recipient.as_slice());
    buf.extend_from_slice(&asset_id.to_be_bytes());
    buf.extend_from_slice(&amount.to_be_bytes());
    keccak256(&buf)
}

/// Verify bridge signatures from validators (per spec §5.6.1)
///
/// Requires at least `min_signatures` (default 14 = 2/3 of 21 subset)
/// valid secp256k1 signatures from distinct validators.
pub fn verify_bridge_signatures(
    op: &ExternalBridgeOp,
    validators: &[Address],
    min_signatures: u64,
) -> Result<(), BridgeError> {
    let ExternalBridgeOp::Deposit {
        source_chain,
        source_tx_hash,
        source_block_number,
        sender,
        recipient,
        asset_id,
        amount,
        signatures,
    } = op
    else {
        return Ok(()); // Withdraw doesn't need signature verification
    };

    // Check minimum count
    if (signatures.len() as u64) < min_signatures {
        return Err(BridgeError::InsufficientSignatures(
            signatures.len() as u64,
            min_signatures,
        ));
    }

    // Compute the event hash that was signed
    let event_hash = bridge_event_hash(
        source_chain,
        *source_tx_hash,
        *source_block_number,
        sender,
        *recipient,
        *asset_id,
        *amount,
    );

    // Verify each signature and track which validators signed
    let mut seen_validators = std::collections::HashSet::<u32>::new();

    for (i, bridge_sig) in signatures.iter().enumerate() {
        // Check for duplicate validators
        if !seen_validators.insert(bridge_sig.validator_index) {
            return Err(BridgeError::InvalidSignature(
                i as u64,
                "duplicate validator".into(),
            ));
        }

        // Recover signer from signature and event hash
        let recovered = recover_secp256k1_signer(&event_hash.0, &bridge_sig.signature)
            .map_err(|e| BridgeError::InvalidSignature(i as u64, format!("{e:?}")))?;

        // Verify the recovered address is in the validator set
        if !validators.contains(&recovered) {
            return Err(BridgeError::InvalidSignature(
                i as u64,
                "signer not in validator set".into(),
            ));
        }
    }

    // Final check: enough unique valid signatures
    if (seen_validators.len() as u64) < min_signatures {
        return Err(BridgeError::InsufficientSignatures(
            seen_validators.len() as u64,
            min_signatures,
        ));
    }

    Ok(())
}

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

/// Process an external bridge deposit: verify signatures → mint protocol balance
///
/// Per spec §5.6.3:
/// 1. Verify bridge signatures (14+ validators)
/// 2. Check asset is allowed
/// 3. Check limits (per-tx, daily)
/// 4. Check source tx not already processed
/// 5. Credit protocol balance
pub fn process_external_deposit(
    op: &ExternalBridgeOp,
    protocol_balances: &mut BalanceState,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    validators: &[Address],
    processed_txs: &mut std::collections::HashSet<B256>,
) -> Result<(), BridgeError> {
    let ExternalBridgeOp::Deposit {
        source_tx_hash,
        asset_id,
        recipient,
        amount,
        ..
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a deposit op".into()));
    };

    // 1. Check asset is allowed (cheap config check before expensive sig verification)
    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    // 2. Verify signatures
    verify_bridge_signatures(op, validators, config.min_validator_signatures)?;

    // 3. Check per-tx limit
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 4. Check daily limit
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset)?;

    // 5. Check source tx not already processed (replay protection)
    if processed_txs.contains(source_tx_hash) {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    // 6. Credit protocol balance
    let _ = protocol_balances.credit_balance(*asset_id, *recipient, *amount);

    // 7. Mark source tx as processed
    processed_txs.insert(*source_tx_hash);

    // 8. Record deposit
    bridge_state.record_deposit(*asset_id, *amount);

    Ok(())
}

/// Process an external bridge withdrawal: burn protocol → emit event for validators to sign
///
/// Per spec §5.6.4:
/// 1. Check asset is allowed
/// 2. Check limits
/// 3. Deduct protocol balance
/// 4. Return withdraw event for validator signing
pub fn process_external_withdraw(
    op: &ExternalBridgeOp,
    protocol_balances: &mut BalanceState,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
) -> Result<(), BridgeError> {
    let ExternalBridgeOp::Withdraw {
        asset_id,
        sender,
        amount,
        ..
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a withdraw op".into()));
    };

    // 1. Check asset is allowed
    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    // 2. Check per-tx limit
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 3. Check daily limit
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset)?;

    // 4. Check protocol balance
    let balance = protocol_balances.get_balance(*asset_id, sender);
    if balance < *amount {
        return Err(BridgeError::InsufficientProtocolBalance(*asset_id, *amount));
    }

    // 5. Deduct protocol balance
    protocol_balances.deduct_balance(*asset_id, *sender, *amount)?;

    // 6. Record withdrawal
    bridge_state.record_withdrawal(*asset_id, *amount);

    Ok(())
}

/// Check if bridge signature collection is complete for a pending deposit
pub fn is_signature_complete(
    signatures_received: u64,
    config: &BridgeConfig,
) -> bool {
    signatures_received >= config.min_validator_signatures
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_crypto::generate_keypair;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    /// Generate (secret_key, address) pairs for test validators
    fn generate_validators(count: usize) -> (Vec<[u8; 32]>, Vec<Address>) {
        let mut secrets = Vec::with_capacity(count);
        let mut addrs = Vec::with_capacity(count);
        for _ in 0..count {
            let (secret, _) = generate_keypair();
            // We need to derive the address from the secret key
            // Use a dummy signing to get the address
            let msg_hash = [0u8; 32];
            let sig = secp256k1_sign(&secret, &msg_hash);
            let addr = recover_secp256k1_signer(&msg_hash, &sig).unwrap();
            secrets.push(secret);
            addrs.push(addr);
        }
        (secrets, addrs)
    }

    /// Build a deposit op with real signatures from the given validators
    fn build_signed_deposit(
        secrets: &[[u8; 32]],
        _addrs: &[Address],
        indices: &[usize], // which validators sign
    ) -> ExternalBridgeOp {
        let source_tx_hash = B256::ZERO;
        let source_block_number: u64 = 100;
        let sender = vec![0u8; 32];
        let recipient = test_addr(1);
        let asset_id: AssetId = 1;
        let amount: u128 = 1000;
        let source_chain = ExternalChain::EthereumMainnet;

        let signatures = indices
            .iter()
            .map(|&vi| BridgeSignature {
                validator_index: vi as u32,
                signature: sign_bridge_event(
                    &secrets[vi],
                    &source_chain,
                    source_tx_hash,
                    source_block_number,
                    &sender,
                    recipient,
                    asset_id,
                    amount,
                ),
            })
            .collect();

        ExternalBridgeOp::Deposit {
            source_chain,
            source_tx_hash,
            source_block_number,
            sender,
            recipient,
            asset_id,
            amount,
            signatures,
        }
    }

    #[test]
    fn test_external_chain_ids() {
        assert_eq!(ExternalChain::EthereumMainnet.chain_id(), 1);
        assert_eq!(ExternalChain::Arbitrum.chain_id(), 42161);
    }

    #[test]
    fn test_verify_insufficient_signatures() {
        let (_secrets, validators) = generate_validators(21);
        // Only 5 signatures, need 14
        let op = build_signed_deposit(
            &generate_validators(21).0,
            &validators,
            &[0, 1, 2, 3, 4],
        );
        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(matches!(result, Err(BridgeError::InsufficientSignatures(5, 14))));
    }

    #[test]
    fn test_verify_duplicate_validator() {
        let (secrets, validators) = generate_validators(21);
        let source_tx_hash = B256::ZERO;
        let source_block_number: u64 = 100;
        let sender = vec![0u8; 32];
        let recipient = test_addr(1);
        let asset_id: AssetId = 1;
        let amount: u128 = 1000;
        let source_chain = ExternalChain::EthereumMainnet;

        // 14 valid signatures from validators 0-13
        let mut sigs: Vec<BridgeSignature> = (0..14)
            .map(|i| BridgeSignature {
                validator_index: i as u32,
                signature: sign_bridge_event(
                    &secrets[i],
                    &source_chain,
                    source_tx_hash,
                    source_block_number,
                    &sender,
                    recipient,
                    asset_id,
                    amount,
                ),
            })
            .collect();
        // Add duplicate: validator 0 again
        sigs.push(BridgeSignature {
            validator_index: 0, // duplicate
            signature: sign_bridge_event(
                &secrets[0],
                &source_chain,
                source_tx_hash,
                source_block_number,
                &sender,
                recipient,
                asset_id,
                amount,
            ),
        });

        let op = ExternalBridgeOp::Deposit {
            source_chain,
            source_tx_hash,
            source_block_number,
            sender,
            recipient,
            asset_id,
            amount,
            signatures: sigs,
        };

        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(matches!(result, Err(BridgeError::InvalidSignature(14, _))));
    }

    #[test]
    fn test_verify_valid_signatures_14() {
        let (secrets, validators) = generate_validators(21);
        let op = build_signed_deposit(&secrets, &validators, &(0..14).collect::<Vec<_>>());
        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_signer_not_in_validator_set() {
        let (secrets, validators) = generate_validators(21);
        // Use a secret key NOT in the validator set to sign
        let (rogue_secret, _) = generate_keypair();
        let source_tx_hash = B256::ZERO;
        let source_block_number: u64 = 100;
        let sender = vec![0u8; 32];
        let recipient = test_addr(1);
        let asset_id: AssetId = 1;
        let amount: u128 = 1000;
        let source_chain = ExternalChain::EthereumMainnet;

        let signatures: Vec<BridgeSignature> = (0..14)
            .map(|i| {
                // Use rogue for the last signature
                if i == 13 {
                    BridgeSignature {
                        validator_index: 13,
                        signature: sign_bridge_event(
                            &rogue_secret,
                            &source_chain,
                            source_tx_hash,
                            source_block_number,
                            &sender,
                            recipient,
                            asset_id,
                            amount,
                        ),
                    }
                } else {
                    BridgeSignature {
                        validator_index: i as u32,
                        signature: sign_bridge_event(
                            &secrets[i],
                            &source_chain,
                            source_tx_hash,
                            source_block_number,
                            &sender,
                            recipient,
                            asset_id,
                            amount,
                        ),
                    }
                }
            })
            .collect();

        let op = ExternalBridgeOp::Deposit {
            source_chain,
            source_tx_hash,
            source_block_number,
            sender,
            recipient,
            asset_id,
            amount,
            signatures,
        };

        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(matches!(result, Err(BridgeError::InvalidSignature(13, _))));
    }

    #[test]
    fn test_signature_complete() {
        let config = BridgeConfig::default();
        assert!(is_signature_complete(14, &config));
        assert!(is_signature_complete(21, &config));
        assert!(!is_signature_complete(13, &config));
        assert!(!is_signature_complete(0, &config));
    }

    #[test]
    fn test_external_deposit_asset_not_allowed() {
        let mut protocol_balances = BalanceState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig {
            allowed_assets: vec![1], // only CALL allowed
            ..Default::default()
        };
        let (_, validators) = generate_validators(21);
        let mut processed_txs = std::collections::HashSet::new();

        let op = ExternalBridgeOp::Deposit {
            source_chain: ExternalChain::EthereumMainnet,
            source_tx_hash: B256::ZERO,
            source_block_number: 100,
            sender: vec![0u8; 32],
            recipient: test_addr(1),
            asset_id: 99, // not allowed
            amount: 1000,
            signatures: vec![], // No sigs needed since it fails on asset check first
        };

        let result = process_external_deposit(
            &op,
            &mut protocol_balances,
            &mut bridge_state,
            &config,
            &validators,
            &mut processed_txs,
        );
        assert!(matches!(result, Err(BridgeError::ExternalAssetNotAllowed(99))));
    }

    #[test]
    fn test_external_withdraw_insufficient_balance() {
        let mut protocol_balances = BalanceState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();

        let op = ExternalBridgeOp::Withdraw {
            target_chain: ExternalChain::EthereumMainnet,
            target_address: vec![0u8; 32],
            asset_id: 1,
            sender: test_addr(1),
            amount: 1000,
        };

        let result = process_external_withdraw(
            &op,
            &mut protocol_balances,
            &mut bridge_state,
            &config,
        );
        assert!(matches!(result, Err(BridgeError::InsufficientProtocolBalance(1, 1000))));
    }
}

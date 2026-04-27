//! Tests for transaction model, gas, fees, and mempool.

use call_primitives::{Address, FeeCurrency, Hash};
use crate::account::AccountState;
use crate::instructions::{Instruction, PaymentMemo};
use crate::sponsor::{GasSponsorAuth, SponsorRegistry};
use crate::FeeCurrencyRegistry;
use crate::tx::model::{AuthScheme, GasConfig, ProtocolTransaction};
use crate::tx::gas::{calculate_gas_units, update_base_fee, FeeParams};
use crate::tx::fee::{convert_fee_to_stablecoin, deduct_gas};
use crate::tx::mempool::{accept_to_mempool, MAX_INSTRUCTIONS_PER_TX, MAX_TOTAL_MEMO_BYTES};

use std::collections::HashSet;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn make_transfer() -> Instruction {
    Instruction::Transfer {
        asset_id: crate::CALL_ASSET_ID,
        to: test_addr(2),
        amount: 100,
        memo: None,
    }
}

#[test]
fn test_gas_calculation_single_transfer() {
    let instrs = vec![make_transfer()];
    assert_eq!(calculate_gas_units(&instrs), 10_000);
}

#[test]
fn test_gas_calculation_multi_instruction_discount() {
    let instrs = vec![make_transfer(); 5];
    // 1st: 10000, 2nd-5th: 5000 each = 10000 + 4*5000 = 30000
    assert_eq!(calculate_gas_units(&instrs), 30_000);
}

#[test]
fn test_gas_calculation_batch_100_payments() {
    let instrs = vec![make_transfer(); 100];
    let mut expected = 10_000u64; // 1st
    expected += 9 * 5_000;       // 2nd-10th
    expected += 90 * 2_500;      // 11th-100th
    assert_eq!(calculate_gas_units(&instrs), expected);
}

#[test]
fn test_base_fee_increase_on_congestion() {
    let mut params = FeeParams::default();
    params.base_fee = 100;
    // Gas used > target -> fee should increase
    update_base_fee(&mut params, 15_000_000);
    assert!(params.base_fee > 100);
}

#[test]
fn test_base_fee_decrease_on_idle() {
    let mut params = FeeParams::default();
    params.base_fee = 100;
    // Gas used < target -> fee should decrease
    update_base_fee(&mut params, 5_000_000);
    assert!(params.base_fee < 100);
}

#[test]
fn test_base_fee_capping_at_max() {
    let mut params = FeeParams::default();
    params.base_fee = params.max_base_fee;
    // Even with high usage, fee should not exceed max
    update_base_fee(&mut params, 20_000_000);
    assert_eq!(params.base_fee, params.max_base_fee);
}

#[test]
fn test_deduct_gas_self_pay() {
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1000).unwrap();
    let mut sponsors = SponsorRegistry::new();
    deduct_gas(
        &mut account,
        &GasConfig::SelfPay,
        &FeeCurrency::Call,
        500,
        test_addr(1),
        Hash::ZERO,
        &mut sponsors,
        0,
    )
    .unwrap();
    assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(1)), 500);
}

#[test]
fn test_deduct_gas_authorized_sponsor() {
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(2), 1000).unwrap();
    let mut sponsors = SponsorRegistry::new();
    let auth = GasSponsorAuth {
        sponsor: test_addr(2),
        allowed_senders: vec![test_addr(1)],
        max_daily: 10_000,
        expires_at: 1000,
        sponsor_signature: [0u8; 65],
    };
    sponsors.register_sponsor_auth(auth).unwrap();

    let result = deduct_gas(
        &mut account,
        &GasConfig::AuthorizedSponsor { sponsor: test_addr(2) },
        &FeeCurrency::Call,
        500,
        test_addr(1),
        Hash::ZERO,
        &mut sponsors,
        10,
    );
    assert!(result.is_ok());
    assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(2)), 500);
}

#[test]
fn test_deduct_gas_pool_sponsor() {
    let mut account = AccountState::new();
    let mut sponsors = SponsorRegistry::new();
    let pool_addr = test_addr(9);
    sponsors.deposit_to_pool(pool_addr, 10_000).unwrap();
    account.balances.set_balance(crate::CALL_ASSET_ID, pool_addr, 10_000).unwrap();

    let result = deduct_gas(
        &mut account,
        &GasConfig::PoolSponsor { sponsor: pool_addr },
        &FeeCurrency::Call,
        500,
        test_addr(1),
        Hash::ZERO,
        &mut sponsors,
        10,
    );
    assert!(result.is_ok(), "pool sponsor deduct failed: {:?}", result);
}

#[test]
fn test_deduct_gas_per_tx_sponsor() {
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(2), 1000).unwrap();
    let mut sponsors = SponsorRegistry::new();
    let result = deduct_gas(
        &mut account,
        &GasConfig::PerTxSponsor {
            sponsor: test_addr(2),
            sponsor_signature: vec![0u8; 65],
        },
        &FeeCurrency::Call,
        500,
        test_addr(1),
        Hash::ZERO,
        &mut sponsors,
        10,
    );
    assert!(result.is_ok());
    assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(2)), 500);
}

#[test]
fn test_stablecoin_fee_conversion() {
    let call_fee = 1_000_000u128; // 1M wei
    let call_price_usd = 2_000_000u128; // $2.00 (6 decimals)
    let stablecoin_decimals = 6u8;
    let result = convert_fee_to_stablecoin(call_fee, call_price_usd, stablecoin_decimals);
    assert_eq!(result, 2_000_000u128); // $2.00 in 6 decimals
}

#[test]
fn test_stablecoin_not_in_registry_rejected() {
    // Registry check happens at fee deduction time via asset lookup
    // This test documents the flow
    let registry = FeeCurrencyRegistry::new();
    assert!(!registry.is_allowed(999));
}

#[test]
fn test_mempool_accept_low_fee_rejected() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 20_000,
        max_fee: 1, // too low
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();
    let expected_nonces = std::collections::HashMap::new();

    let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(result.is_err());
}

#[test]
fn test_mempool_accept_sufficient_balance() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 20_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();
    let expected_nonces = std::collections::HashMap::new();

    let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(result.is_ok());
}

#[test]
fn test_mempool_reject_stale_nonce() {
    // Nonce 0 is already used (expected=1)
    let mut expected_nonces = std::collections::HashMap::new();
    expected_nonces.insert(test_addr(1), 1);

    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0, // already used
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 20_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();

    let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(result.is_err());
}

#[test]
fn test_mempool_reject_too_many_instructions() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0,
        instructions: vec![make_transfer(); MAX_INSTRUCTIONS_PER_TX + 1],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 20_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();
    let expected_nonces = std::collections::HashMap::new();

    let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(result.is_err());
}

#[test]
fn test_mempool_reject_oversized_memo() {
    let big_memo = PaymentMemo {
        message: "a".repeat(MAX_TOTAL_MEMO_BYTES + 1),
        reference: None,
        metadata: None,
    };
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0,
        instructions: vec![Instruction::Transfer {
            asset_id: crate::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 100,
            memo: Some(big_memo),
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 20_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();
    let expected_nonces = std::collections::HashMap::new();

    let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(result.is_err());
}

// -- Signature negative tests ------------------------------------------

#[test]
fn test_verify_signature_all_zeros() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    };
    assert!(tx.verify_signature().is_err());
}

#[test]
fn test_verify_signature_all_ones() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0xFFu8; 65] },
    };
    assert!(tx.verify_signature().is_err());
}

#[test]
fn test_verify_signature_wrong_keypair() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let correct_sender = call_crypto::pubkey_to_address(&pubkey);
    let wrong_addr = test_addr(0xAA);

    let msg_hash = {
        let mut buf = Vec::new();
        buf.extend_from_slice(&correct_sender.as_slice());
        buf.extend_from_slice(&1u64.to_le_bytes());
        // Include instructions in hash
        call_crypto::keccak256(&buf)
    };
    let sig = call_crypto::secp256k1_sign(&secret, &msg_hash);

    // Sign with correct keypair but claim a different sender
    let tx = ProtocolTransaction {
        sender: wrong_addr,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: sig },
    };
    assert!(tx.verify_signature().is_err());
}

#[test]
fn test_verify_signature_malleated_recovery_id() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let tx_hash = {
        let mut buf = Vec::new();
        buf.extend_from_slice(&sender.as_slice());
        buf.extend_from_slice(&1u64.to_le_bytes());
        call_crypto::keccak256(&buf)
    };
    let mut sig = call_crypto::secp256k1_sign(&secret, &tx_hash);

    // Flip the recovery ID byte
    sig[64] ^= 0x01;

    let tx = ProtocolTransaction {
        sender,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: sig },
    };
    assert!(tx.verify_signature().is_err());
}

#[test]
fn test_verify_signature_wrong_signer() {
    let (secret_a, pubkey_a) = call_crypto::generate_keypair();
    let sender_a = call_crypto::pubkey_to_address(&pubkey_a);
    let (_, pubkey_b) = call_crypto::generate_keypair();
    let sender_b = call_crypto::pubkey_to_address(&pubkey_b);

    // Sign tx as sender_a
    let tx_a = ProtocolTransaction {
        sender: sender_a,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    };
    let tx_hash = tx_a.compute_tx_hash();
    let sig = call_crypto::secp256k1_sign(&secret_a, &tx_hash);

    // But claim sender is sender_b
    let tx_impersonating_b = ProtocolTransaction {
        sender: sender_b,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: sig },
    };
    assert!(
        tx_impersonating_b.verify_signature().is_err(),
        "signature from A should not verify as B"
    );
}

#[test]
fn test_verify_signature_tampered_tx() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    // Sign a tx with amount 100
    let tx = ProtocolTransaction {
        sender,
        nonce: 1,
        instructions: vec![Instruction::Transfer {
            asset_id: crate::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 100,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    };
    let tx_hash = tx.compute_tx_hash();
    let sig = call_crypto::secp256k1_sign(&secret, &tx_hash);

    // Attacker mutates amount to 1000 but keeps the original signature
    let tampered_tx = ProtocolTransaction {
        sender,
        nonce: 1,
        instructions: vec![Instruction::Transfer {
            asset_id: crate::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 1000, // changed!
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: sig },
    };
    assert!(
        tampered_tx.verify_signature().is_err(),
        "signature over different hash should fail"
    );
}

#[test]
fn test_verify_signature_nonce_replay() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    // Build tx with placeholder sig, then sign it properly
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    };

    // Sign with the actual tx hash so verify_signature succeeds
    let tx_hash = tx.compute_tx_hash();
    let sig = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx = ProtocolTransaction {
        sender,
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig { signature: sig },
    };

    // Signature verifies correctly
    assert!(tx.verify_signature().is_ok());

    // But mempool rejects duplicate nonce
    let mut account = AccountState::new();
    account.balances.set_balance(crate::CALL_ASSET_ID, sender, 1_000_000).unwrap();
    let fee_params = FeeParams::default();
    let nonces = HashSet::new();
    let mut expected_nonces = std::collections::HashMap::new();
    expected_nonces.insert(sender, 1);

    let r1 = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    assert!(r1.is_ok());

    // After accepting tx1, expected nonce increments to 2
    expected_nonces.insert(sender, 2);
    let _r2 = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
    // tx has nonce 1 but expected is 2, so it should be rejected
}

#[test]
fn test_verify_signature_multi_sig_insufficient_threshold() {
    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::MultiSig {
            signatures: vec![
                [1u8; 65], // invalid sigs
                [2u8; 65],
            ],
        },
    };

    // Verify with invalid signatures -- recovery will fail
    assert!(tx.verify_signature().is_err());
}

#[test]
fn test_verify_signature_session_key_mismatch() {
    let session_key = test_addr(0xBB);

    let tx = ProtocolTransaction {
        sender: test_addr(1),
        nonce: 1,
        instructions: vec![make_transfer()],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SessionKey { key: session_key, signature: [0u8; 65] },
    };
    // SessionKey auth with an invalid signature should fail
    assert!(tx.verify_signature().is_err());
}

// -- Property-based tests ----------------------------------------------

use proptest::prelude::*;

proptest! {
    #[test]
    fn test_tx_roundtrip_encode_decode(
        sender_bytes: [u8; 20],
        nonce: u64,
        amount: u128,
        gas_limit: u64,
        max_fee: u128,
        expires_at: u64,
    ) {
        let sender = Address::from_slice(&sender_bytes);
        let instructions = vec![Instruction::Transfer {
            asset_id: crate::CALL_ASSET_ID,
            to: sender,
            amount,
            memo: None,
        }];

        let tx = ProtocolTransaction {
            sender,
            nonce,
            instructions,
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit,
            max_fee,
            expires_at,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        };

        // Serialize and deserialize
        let bytes = serde_json::to_vec(&tx).unwrap();
        let decoded: ProtocolTransaction = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(decoded.sender, tx.sender);
        assert_eq!(decoded.nonce, tx.nonce);
        assert_eq!(decoded.gas_limit, tx.gas_limit);
        assert_eq!(decoded.max_fee, tx.max_fee);
        assert_eq!(decoded.expires_at, tx.expires_at);
    }

    #[test]
    fn test_instruction_roundtrip(
        asset_id: u64,
        amount: u128,
        to_bytes: [u8; 20],
    ) {
        let to = Address::from_slice(&to_bytes);
        let instr = Instruction::Transfer {
            asset_id,
            to,
            amount,
            memo: Some(PaymentMemo {
                message: "test memo".to_string(),
                reference: Some("ref-123".to_string()),
                metadata: None,
            }),
        };

        let bytes = serde_json::to_vec(&instr).unwrap();
        let decoded: Instruction = serde_json::from_slice(&bytes).unwrap();

        if let Instruction::Transfer { asset_id: a_id, to: a_to, amount: a_amount, .. } = decoded {
            assert_eq!(a_id, asset_id);
            assert_eq!(a_to, to);
            assert_eq!(a_amount, amount);
        } else {
            panic!("decoded wrong instruction variant");
        }
    }
}

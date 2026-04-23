//! Integration test shared utilities (T22.1)

use call_primitives::{Address, AssetId, Balance, FeeCurrency, Hash};
use call_protocol::AccountState;
use call_protocol::compliance::ComplianceEngine;
use call_protocol::instructions::{
    AgentPayment, Instruction, PaymentEntry, PaymentMemo,
};
use call_protocol::registry::{AssetRegistry, AssetStatus};
use call_protocol::smart_accounts::{
    MultiSigConfig, SessionPermissions, SmartAccountRegistry,
};
use call_protocol::sponsor::{GasSponsorAuth, GasSponsorPool, SponsorRegistry};
use call_protocol::transaction::{
    accept_to_mempool, calculate_gas_units, compute_fee, deduct_gas, update_base_fee,
    AuthScheme, FeeParams, GasConfig, MempoolConfig,
};
use call_protocol::ProtocolTransaction;
use call_shielded::ShieldedState;
use std::collections::{HashMap, HashSet};

// ── Helpers ─────────────────────────────────────────────────────────────

pub fn addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

pub fn hash_byte(n: u8) -> Hash {
    Hash::repeat_byte(n)
}

pub fn sig_byte(n: u8) -> [u8; 65] {
    [n; 65]
}

pub fn make_tx(
    sender: Address,
    nonce: u64,
    instructions: Vec<Instruction>,
    gas_config: GasConfig,
) -> ProtocolTransaction {
    ProtocolTransaction {
        sender,
        nonce,
        instructions,
        gas_config,
        fee_currency: FeeCurrency::Call,
        gas_limit: 10_000_000,
        max_fee: 1_000_000_000,
            expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: sig_byte(0xAA),
        },
    }
}

pub fn make_transfer(asset_id: AssetId, to: Address, amount: Balance) -> Instruction {
    Instruction::Transfer {
        asset_id,
        to,
        amount,
        memo: None,
    }
}

pub fn make_memo() -> PaymentMemo {
    PaymentMemo {
        message: "integration test".into(),
        reference: Some("ref-001".into()),
        metadata: None,
    }
}

/// Setup: register an asset and credit initial balance
pub fn setup_asset(
    account: &mut AccountState,
    registry: &mut AssetRegistry,
    symbol: &str,
    issuer: Address,
    holder: Address,
    initial: Balance,
) -> AssetId {
    // Register CALL first so user assets get IDs >= 2 and don't collide with gas asset.
    if registry.get_asset(call_protocol::CALL_ASSET_ID).is_none() {
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, issuer, 0, 100)
            .ok();
    }
    let id = registry
        .register_asset(
            symbol.into(),
            format!("{symbol} Token"),
            18,
            issuer,
            0, // no compliance
            100, // registered_at
        )
        .unwrap();
    account.balances.set_balance(id, holder, initial).unwrap();
    // Also give the issuer and holder some CALL balance for fee payment.
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, issuer, 1_000_000_000).unwrap();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, holder, 1_000_000_000).unwrap();
    id
}

/// Execute a protocol transaction through the full instruction pipeline
pub fn execute_tx(
    tx: &ProtocolTransaction,
    account: &mut AccountState,
    registry: &mut AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    fee_params: &FeeParams,
) -> Result<(), String> {
    // Mempool acceptance
    let nonces = HashSet::new();
    let expected_nonces = HashMap::new();
    accept_to_mempool(tx, account, fee_params, &nonces, &expected_nonces)
        .map_err(|e| format!("mempool reject: {e}"))?;

    // Gas calculation
    let gas_units = calculate_gas_units(&tx.instructions);
    let fee = compute_fee(gas_units, 0, fee_params.base_fee);

    // Deduct gas
    let mut sponsor_registry = SponsorRegistry::new();
    deduct_gas(
        account,
        &tx.gas_config,
        &tx.fee_currency,
        fee,
        tx.sender,
        hash_byte(0x01),
        &mut sponsor_registry,
        0,
    )
    .map_err(|e| format!("gas deduct: {e}"))?;

    // Execute instructions
    call_protocol::instructions::execute_protocol_instructions(
        &tx.instructions,
        account,
        registry,
        compliance,
        shielded_state,
        tx.sender,
        None,
        &mut None,
        None,
    )
    .map_err(|e| format!("exec: {e}"))?;

    Ok(())
}

/// Setup a sponsor with balance and whitelist
pub fn setup_sponsor(
    sponsors: &mut SponsorRegistry,
    account: &mut AccountState,
    sponsor: Address,
    allowed_senders: Vec<Address>,
    max_daily: u128,
) {
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sponsor, 100_000_000)
        .unwrap();
    let auth = GasSponsorAuth {
        sponsor,
        allowed_senders,
        max_daily,
        expires_at: u64::MAX,
        sponsor_signature: sig_byte(0xBB),
    };
    sponsors.register_sponsor_auth(auth).unwrap();
}

/// Setup multi-sig account
pub fn setup_multisig(
    registry: &mut SmartAccountRegistry,
    account: Address,
    signers: Vec<Address>,
    threshold: u32,
) {
    registry
        .register_multi_sig(account, signers, threshold)
        .unwrap();
}

/// Setup session key
pub fn setup_session_key(
    registry: &mut SmartAccountRegistry,
    account: Address,
    session_key: Address,
    expires_at: u64,
) {
    registry
        .create_session_key(
            account,
            session_key,
            SessionPermissions {
                allowed_instructions: vec![
                    call_primitives::InstructionType::Transfer,
                    call_primitives::InstructionType::BatchTransfer,
                ],
                max_per_tx: 10_000,
                max_daily: 100_000,
                allowed_targets: vec![],
                allowed_assets: vec![],
            },
            expires_at,
        )
        .unwrap();
}

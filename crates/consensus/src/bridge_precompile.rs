//! Bridge precompile implementation for `call-consensus`.
//!
//! Precompile at address `0x103` providing:
//! - getTotalDeposits() -> uint256
//! - getTotalWithdrawals() -> uint256
//! - externalBridgeDeposit(bytes32,uint64,uint64,bytes,address,uint64,uint256,bytes)
//! - externalBridgeWithdraw(uint64,bytes,uint64,uint256)
//! - challengeBridgeDeposit(bytes32)

use std::cell::RefCell;

use call_precompiles::state_hook::with_account_state;
use call_precompiles::{
    current_caller, PrecompileError, PrecompileOutput,
    PrecompileResult,
};
use call_primitives::{Address, B256};

// ── Thread-local bridge state reference ───────────────────────────────

thread_local! {
    static TL_BRIDGE_STATE: RefCell<Option<BridgeStateRef>> = RefCell::new(None);
}

#[derive(Clone, Copy)]
struct BridgeStateRef {
    bridge_state: *mut call_bridge::BridgeStateManager,
    bridge_config: *const call_bridge::BridgeConfig,
    validators: *const Address,
    validator_count: usize,
    current_block: u64,
}

// Safety: BridgeStateRef is !Send and !Sync because it contains raw
// pointers, and we only use it via thread-local storage.

/// Guard that injects bridge state into thread-local storage for precompiles.
pub struct BridgeStateHookGuard;

impl BridgeStateHookGuard {
    /// Inject bridge state into the precompile hook layer.
    ///
    /// # Safety
    /// The caller must ensure that all passed references outlive this guard.
    pub unsafe fn from_raw(
        bridge_state: *mut call_bridge::BridgeStateManager,
        bridge_config: *const call_bridge::BridgeConfig,
        validators: *const Address,
        validator_count: usize,
        current_block: u64,
    ) -> Self {
        let refs = BridgeStateRef {
            bridge_state,
            bridge_config,
            validators,
            validator_count,
            current_block,
        };
        TL_BRIDGE_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }
}

impl Drop for BridgeStateHookGuard {
    fn drop(&mut self) {
        TL_BRIDGE_STATE.with(|t| *t.borrow_mut() = None);
    }
}

/// Access the bridge state (if hooked).
fn with_bridge_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(
        &mut call_bridge::BridgeStateManager,
        Option<&call_bridge::BridgeConfig>,
        &[Address],
        u64,
    ) -> R,
{
    TL_BRIDGE_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| unsafe {
            let config = if r.bridge_config.is_null() {
                None
            } else {
                Some(&*r.bridge_config)
            };
            let validators = if r.validators.is_null() || r.validator_count == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(r.validators, r.validator_count)
            };
            f(&mut *r.bridge_state, config, validators, r.current_block)
        })
    })
}

// ── ABI decoding helpers ──────────────────────────────────────────────

fn require_caller() -> Result<Address, PrecompileError> {
    current_caller().ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    let val = u64::from_be_bytes(buf);
    Some(val as usize)
}

fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset; // args start at byte 4
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let data_start = abs_offset + 32;
    if input.len() < data_start + len {
        return None;
    }
    Some(input[data_start..data_start + len].to_vec())
}

fn decode_b256(input: &[u8], offset: usize) -> Option<B256> {
    if input.len() < offset + 32 {
        return None;
    }
    Some(B256::from_slice(&input[offset..offset + 32]))
}

fn decode_address(input: &[u8], slot_offset: usize) -> Option<Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(Address::from_slice(&input[start..start + 20]))
}

// ── Bridge precompile entry point ─────────────────────────────────────

/// No-op: bridge precompile is now stateful and self-contained.
#[deprecated(note = "bridge precompile is stateful; registration no longer needed")]
pub fn register_bridge_precompile() {}

pub fn bridge_ext_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0xa8, 0x7e, 0x4f, 0x2a] => get_total_deposits(input, gas_limit),
        &[0x9c, 0x3e, 0x6d, 0x1b] => get_total_withdrawals(input, gas_limit),
        &[0xb2, 0x23, 0x36, 0x65] => external_bridge_deposit(input, gas_limit),
        &[0x2c, 0xcf, 0xe9, 0x9f] => external_bridge_withdraw(input, gas_limit),
        &[0x51, 0xf6, 0xcb, 0xbf] => challenge_bridge_deposit(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── getTotalDeposits() -> uint256 ────────────────────────────────────

fn get_total_deposits(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1500;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    let total = with_bridge_state(|bridge, _config, _vals, _block| {
        bridge.total_deposits.values().copied().sum::<u128>()
    })
    .unwrap_or(0);

    let mut output = [0u8; 32];
    output[16..32].copy_from_slice(&total.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── getTotalWithdrawals() -> uint256 ─────────────────────────────────

fn get_total_withdrawals(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1500;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    let total = with_bridge_state(|bridge, _config, _vals, _block| {
        bridge.total_withdrawals.values().copied().sum::<u128>()
    })
    .unwrap_or(0);

    let mut output = [0u8; 32];
    output[16..32].copy_from_slice(&total.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── externalBridgeDeposit(...) ────────────────────────────────────────

fn external_bridge_deposit(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 50000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 260 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let source_tx_hash = decode_b256(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid source_tx_hash".into())
    })?;
    let source_chain_id = decode_u64(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid source_chain".into())
    })?;
    let source_block_number = decode_u64(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid source_block_number".into())
    })?;
    let external_sender = decode_bytes(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid external_sender".into())
    })?;
    let recipient = decode_address(input, 132).ok_or_else(|| {
        PrecompileError::Other("invalid recipient".into())
    })?;
    let asset_id = decode_u64(input, 164).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let amount = decode_u128(input, 196).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;
    let signatures_bytes = decode_bytes(input, 228).ok_or_else(|| {
        PrecompileError::Other("invalid signatures".into())
    })?;

    // Decode signatures: each entry is 4 bytes (index) + 65 bytes (sig)
    let mut signatures = Vec::new();
    let sig_entry_len = 69usize;
    if !signatures_bytes.is_empty() && signatures_bytes.len() % sig_entry_len != 0 {
        return Err(PrecompileError::Other(
            "signatures length must be multiple of 69".into(),
        ));
    }
    for chunk in signatures_bytes.chunks(sig_entry_len) {
        let mut idx_buf = [0u8; 4];
        idx_buf.copy_from_slice(&chunk[0..4]);
        let validator_index = u32::from_be_bytes(idx_buf);
        let mut sig = [0u8; 65];
        sig.copy_from_slice(&chunk[4..69]);
        signatures.push(call_bridge::BridgeSignature {
            validator_index,
            signature: sig,
        });
    }

    let source_chain = match source_chain_id {
        0 => call_bridge::ExternalChain::EthereumMainnet,
        1 => call_bridge::ExternalChain::Arbitrum,
        _ => {
            return Err(PrecompileError::Other("unknown source chain".into()));
        }
    };

    let op = call_bridge::ExternalBridgeOp::Deposit {
        source_chain,
        source_tx_hash,
        source_block_number,
        sender: external_sender,
        recipient,
        asset_id,
        amount,
        signatures,
    };

    with_bridge_state(|bridge_state, config, validators, current_block| {
        let config = config.ok_or_else(|| {
            PrecompileError::Other("bridge config not available".into())
        })?;
        let account = with_account_state(|acc| acc as *mut call_protocol::AccountState)
            .ok_or_else(|| PrecompileError::Other("account state not available".into()))?;
        // SAFETY: The pointer is valid because StateHookGuard is alive
        let acc_ref = unsafe { &mut *account };
        call_bridge::process_external_deposit(
            &op,
            acc_ref,
            bridge_state,
            config,
            validators,
            current_block,
            None, // source_contract: not provided; registry check used
        )
        .map_err(|e| PrecompileError::Other(format!("externalBridgeDeposit: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("bridge state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── externalBridgeWithdraw(uint64 targetChain, bytes targetAddress, uint64 assetId, uint256 amount) ─

fn external_bridge_withdraw(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 30000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let target_chain_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid target_chain".into())
    })?;
    let target_address = decode_bytes(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid target_address".into())
    })?;
    let asset_id = decode_u64(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let amount = decode_u128(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let caller = require_caller()?;

    let target_chain = match target_chain_id {
        0 => call_bridge::ExternalChain::EthereumMainnet,
        1 => call_bridge::ExternalChain::Arbitrum,
        _ => {
            return Err(PrecompileError::Other("unknown target chain".into()));
        }
    };

    let op = call_bridge::ExternalBridgeOp::Withdraw {
        target_chain,
        target_address,
        asset_id,
        sender: caller,
        amount,
    };

    with_bridge_state(|bridge_state, config, _validators, current_block| {
        let config = config.ok_or_else(|| {
            PrecompileError::Other("bridge config not available".into())
        })?;
        let account = with_account_state(|acc| acc as *mut call_protocol::AccountState)
            .ok_or_else(|| PrecompileError::Other("account state not available".into()))?;
        let acc_ref = unsafe { &mut *account };
        call_bridge::process_external_withdraw(
            &op,
            acc_ref,
            bridge_state,
            config,
            current_block,
        )
        .map_err(|e| PrecompileError::Other(format!("externalBridgeWithdraw: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("bridge state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── challengeBridgeDeposit(bytes32 sourceTxHash) ──────────────────────

fn challenge_bridge_deposit(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let source_tx_hash = decode_b256(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid source_tx_hash".into())
    })?;

    with_bridge_state(|bridge_state, _config, _vals, current_block| {
        let revoked =
            call_bridge::challenge_pending_deposit(bridge_state, &source_tx_hash, current_block);
        if revoked {
            Ok(())
        } else {
            Err(PrecompileError::Other(
                "no pending deposit found for source_tx_hash".into(),
            ))
        }
    })
    .ok_or_else(|| PrecompileError::Other("bridge state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_protocol::AccountState;

    fn setup_state_hook(
        account: &mut AccountState,
        bridge_state: &mut call_bridge::BridgeStateManager,
        config: &call_bridge::BridgeConfig,
        validators: &[Address],
        current_block: u64,
    ) -> (call_precompiles::state_hook::StateHookGuard, BridgeStateHookGuard) {
        let mut registry = call_protocol::registry::AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::default();
        let mut shielded = call_shielded::ShieldedState::new();

        let state_guard = unsafe {
            call_precompiles::state_hook::StateHookGuard::from_raw(
                account,
                &mut registry,
                &mut compliance,
                &mut shielded,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        let bridge_guard = unsafe {
            BridgeStateHookGuard::from_raw(
                bridge_state,
                config as *const _,
                validators.as_ptr(),
                validators.len(),
                current_block,
            )
        };
        (state_guard, bridge_guard)
    }

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_get_total_deposits_and_withdrawals() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let config = call_bridge::BridgeConfig::default();

        bridge_state.record_deposit(1, 1000);
        bridge_state.record_deposit(2, 500);
        bridge_state.record_withdrawal(1, 300);

        let (_sg, _bg) = setup_state_hook(&mut account, &mut bridge_state, &config, &[], 100);

        // getTotalDeposits
        let result = bridge_ext_precompile_fn(&[0xa8, 0x7e, 0x4f, 0x2a], 10000);
        assert!(result.is_ok(), "getTotalDeposits failed: {:?}", result);
        let output = result.unwrap().bytes;
        let total = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&output[16..32]);
            buf
        });
        assert_eq!(total, 1500);

        // getTotalWithdrawals
        let result = bridge_ext_precompile_fn(&[0x9c, 0x3e, 0x6d, 0x1b], 10000);
        assert!(result.is_ok(), "getTotalWithdrawals failed: {:?}", result);
        let output = result.unwrap().bytes;
        let total = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&output[16..32]);
            buf
        });
        assert_eq!(total, 300);
    }

    #[test]
    fn test_external_bridge_deposit_insufficient_signatures() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let config = call_bridge::BridgeConfig::default();
        let caller = test_addr(1);

        let (_sg, _bg) =
            setup_state_hook(&mut account, &mut bridge_state, &config, &[], 100);
        call_precompiles::set_current_caller(Some(caller));

        // Encode deposit with empty signatures
        let mut input = vec![0u8; 260];
        input[0..4].copy_from_slice(&[0xb2, 0x23, 0x36, 0x65]);
        // sourceTxHash at 4..36
        input[4..36].copy_from_slice(&[1u8; 32]);
        // sourceChain = 0 at offset 36
        input[60..68].copy_from_slice(&0u64.to_be_bytes());
        // sourceBlockNumber = 1 at offset 68
        input[92..100].copy_from_slice(&1u64.to_be_bytes());
        // externalSender offset = 96 (relative to args start) at offset 100
        // externalSender is empty bytes: offset=96, len=0 at abs_offset=100
        input[124..132].copy_from_slice(&96u64.to_be_bytes());
        // recipient at offset 132
        input[156..176].copy_from_slice(&caller.as_slice());
        // assetId = 1 at offset 164
        input[188..196].copy_from_slice(&1u64.to_be_bytes());
        // amount = 1000 at offset 196
        input[212..228].copy_from_slice(&1000u128.to_be_bytes());
        // signatures offset = 128 (relative to args start) at offset 228
        // signatures is empty bytes: offset=128, len=0 at abs_offset=132
        input[252..260].copy_from_slice(&128u64.to_be_bytes());

        let result = bridge_ext_precompile_fn(&input, 100000);
        assert!(
            result.is_err(),
            "should fail with insufficient signatures: {:?}",
            result
        );

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_external_bridge_withdraw_insufficient_balance() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let config = call_bridge::BridgeConfig::default();
        let caller = test_addr(1);

        let (_sg, _bg) =
            setup_state_hook(&mut account, &mut bridge_state, &config, &[], 100);
        call_precompiles::set_current_caller(Some(caller));

        // Encode withdraw
        let mut input = vec![0u8; 132];
        input[0..4].copy_from_slice(&[0x2c, 0xcf, 0xe9, 0x9f]);
        // targetChain = 0 at offset 4
        input[28..36].copy_from_slice(&0u64.to_be_bytes());
        // targetAddress offset = 96 at offset 36
        // targetAddress is empty bytes at abs_offset=100
        input[60..68].copy_from_slice(&96u64.to_be_bytes());
        // assetId = 1 at offset 68
        input[92..100].copy_from_slice(&1u64.to_be_bytes());
        // amount = 1000 at offset 100
        input[116..132].copy_from_slice(&1000u128.to_be_bytes());

        let result = bridge_ext_precompile_fn(&input, 100000);
        assert!(
            result.is_err(),
            "should fail with insufficient balance: {:?}",
            result
        );

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_challenge_bridge_deposit_no_pending() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let config = call_bridge::BridgeConfig::default();

        let (_sg, _bg) =
            setup_state_hook(&mut account, &mut bridge_state, &config, &[], 100);

        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0x51, 0xf6, 0xcb, 0xbf]);
        input[4..36].copy_from_slice(&[2u8; 32]);

        let result = bridge_ext_precompile_fn(&input, 100000);
        assert!(
            result.is_err(),
            "should fail when no pending deposit: {:?}",
            result
        );
    }

    #[test]
    fn test_challenge_bridge_deposit_success() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let config = call_bridge::BridgeConfig::default();

        let source_tx_hash = B256::from([3u8; 32]);

        // Queue a pending deposit directly
        bridge_state.queue_external_deposit(
            source_tx_hash,
            test_addr(1),
            1,
            1000,
            50,
            1,
        );

        let (_sg, _bg) =
            setup_state_hook(&mut account, &mut bridge_state, &config, &[], 100);

        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0x51, 0xf6, 0xcb, 0xbf]);
        input[4..36].copy_from_slice(&source_tx_hash.0);

        let result = bridge_ext_precompile_fn(&input, 100000);
        assert!(result.is_ok(), "challenge failed: {:?}", result);

        // Verify deposit was removed
        assert!(!bridge_state.has_pending_external_deposit(&source_tx_hash));
    }

    #[test]
    fn test_external_bridge_deposit_with_valid_signatures() {
        let mut account = AccountState::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut config = call_bridge::BridgeConfig::default();
        // Allow asset 1
        config.allowed_assets = vec![1];
        // Set min signatures to 1 for test
        config.min_validator_signatures = 1;
        let caller = test_addr(1);

        // Generate a validator keypair and signature
        let secret_key = [1u8; 32];
        let source_tx_hash = B256::from([4u8; 32]);
        let event_hash = call_bridge::bridge_event_hash(
            &call_bridge::ExternalChain::EthereumMainnet,
            source_tx_hash,
            1,
            &[],
            caller,
            1,
            1000,
        );
        let signature = call_crypto::secp256k1_sign(&secret_key, &event_hash.0);
        let validator_addr =
            call_crypto::recover_secp256k1_signer(&event_hash.0, &signature).unwrap();
        let validators = vec![validator_addr];

        let (_sg, _bg) =
            setup_state_hook(&mut account, &mut bridge_state, &config, &validators, 100);
        call_precompiles::set_current_caller(Some(caller));

        // Build signatures bytes: 4 bytes index + 65 bytes sig
        let mut sigs_bytes = vec![0u8; 69];
        sigs_bytes[0..4].copy_from_slice(&0u32.to_be_bytes());
        sigs_bytes[4..69].copy_from_slice(&signature);

        // Calculate offsets for dynamic fields
        // Static section: 4 + 8*32 = 260 bytes
        // externalSender dynamic data starts at byte 260 (after static section)
        //   offset = 260 - 4 = 256
        //   length = 0 (32 bytes), ends at byte 292
        // signatures dynamic data starts at byte 292
        //   offset = 292 - 4 = 288
        //   length = 69 (32 bytes), data = 69 bytes, ends at byte 393
        let ext_sender_offset = 256u64;
        let sigs_offset = 288u64;

        let mut input = vec![0u8; 393]; // static + externalSender header + signatures header + sigs data
        input[0..4].copy_from_slice(&[0xb2, 0x23, 0x36, 0x65]);
        input[4..36].copy_from_slice(&source_tx_hash.0);
        input[60..68].copy_from_slice(&0u64.to_be_bytes()); // sourceChain
        input[92..100].copy_from_slice(&1u64.to_be_bytes()); // sourceBlockNumber
        input[124..132].copy_from_slice(&ext_sender_offset.to_be_bytes()); // externalSender offset
        input[144..164].copy_from_slice(&caller.as_slice()); // recipient (address is last 20 bytes of 32-byte slot)
        input[188..196].copy_from_slice(&1u64.to_be_bytes()); // assetId
        input[212..228].copy_from_slice(&1000u128.to_be_bytes()); // amount
        input[252..260].copy_from_slice(&sigs_offset.to_be_bytes()); // signatures offset

        // externalSender at abs 260 (offset 256 from args start)
        // length = 0 (written to last 8 bytes of 32-byte slot)
        input[284..292].copy_from_slice(&0u64.to_be_bytes());

        // signatures at abs 292 (offset 288 from args start)
        // length = 69 (written to last 8 bytes of 32-byte slot)
        input[316..324].copy_from_slice(&69u64.to_be_bytes());
        input[324..393].copy_from_slice(&sigs_bytes);

        let result = bridge_ext_precompile_fn(&input, 100000);
        assert!(result.is_ok(), "deposit failed: {:?}", result);

        // Verify deposit was queued
        assert!(bridge_state.has_pending_external_deposit(&source_tx_hash));

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_verify_bridge_sig_standalone() {
        let secret_key = [1u8; 32];
        let source_tx_hash = B256::from([4u8; 32]);
        let caller = test_addr(1);
        let event_hash = call_bridge::bridge_event_hash(
            &call_bridge::ExternalChain::EthereumMainnet,
            source_tx_hash,
            1,
            &[],
            caller,
            1,
            1000,
        );
        let signature = call_crypto::secp256k1_sign(&secret_key, &event_hash.0);
        let validator_addr =
            call_crypto::recover_secp256k1_signer(&event_hash.0, &signature).unwrap();

        let op = call_bridge::ExternalBridgeOp::Deposit {
            source_chain: call_bridge::ExternalChain::EthereumMainnet,
            source_tx_hash,
            source_block_number: 1,
            sender: vec![],
            recipient: caller,
            asset_id: 1,
            amount: 1000,
            signatures: vec![call_bridge::BridgeSignature {
                validator_index: 0,
                signature,
            }],
        };
        let validators = vec![validator_addr];
        let result = call_bridge::verify_bridge_signatures(&op, &validators, 1);
        assert!(result.is_ok(), "verify failed: {:?}", result);
    }
}

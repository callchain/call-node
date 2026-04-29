//! Validator precompile implementation for `call-consensus`.
//!
//! This module provides the real validator precompile logic at address `0x204`.
//! It is registered via `call_precompiles::set_validator_precompile_fn` to avoid
//! cyclic dependencies (call-precompiles cannot depend on call-consensus).
//!
//! Uses thread-local scoped references to access `ValidatorStateManager` and
//! `AccountState` without acquiring locks (locks are already held by the caller
//! in `Block::execute`).

use std::cell::RefCell;

use call_precompiles::state_hook::with_account_state;
use call_precompiles::{current_caller, set_validator_precompile_fn, PrecompileError, PrecompileOutput, PrecompileResult};
use call_primitives::{Address, Ed25519PublicKey};

use crate::validator::ValidatorStateManager;
use crate::STAKING_ESCROW;

// ── Thread-local validator state reference ────────────────────────────

thread_local! {
    static TL_VALIDATOR_STATE: RefCell<Option<ValidatorStateRef>> = RefCell::new(None);
}

#[derive(Clone, Copy)]
struct ValidatorStateRef {
    validator_state: *mut ValidatorStateManager,
    current_block: u64,
}

// Safety: ValidatorStateRef is !Send and !Sync because it contains raw
// pointers, and we only use it via thread-local storage.

/// Guard that injects validator state into thread-local storage for precompiles.
///
/// Create this at the start of `Block::execute` (when validator_state is
/// present) and let it drop at the end.
pub struct ValidatorStateHookGuard;

impl ValidatorStateHookGuard {
    /// Inject validator state into the precompile hook layer.
    ///
    /// # Safety
    /// The caller must ensure that `validator_state` outlives this guard.
    /// This is naturally guaranteed when called from `Block::execute`.
    pub fn new(validator_state: &mut ValidatorStateManager, current_block: u64) -> Self {
        let refs = ValidatorStateRef {
            validator_state,
            current_block,
        };
        TL_VALIDATOR_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }
}

impl Drop for ValidatorStateHookGuard {
    fn drop(&mut self) {
        TL_VALIDATOR_STATE.with(|t| *t.borrow_mut() = None);
    }
}

/// Access the validator state (if hooked).
fn with_validator_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ValidatorStateManager, u64) -> R,
{
    TL_VALIDATOR_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref()
            .map(|r| unsafe { f(&mut *r.validator_state, r.current_block) })
    })
}

// ── ABI decoding helpers ──────────────────────────────────────────────

fn require_caller() -> Result<Address, PrecompileError> {
    current_caller().ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

fn decode_u256(input: &[u8], slot_offset: usize) -> Option<u128> {
    // uint256 in ABI, but we treat it as uint128 for protocol balance compatibility
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

fn decode_u32(input: &[u8], slot_offset: usize) -> Option<u32> {
    let start = slot_offset + 28;
    if input.len() < start + 4 {
        return None;
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&input[start..start + 4]);
    Some(u32::from_be_bytes(buf))
}

// ── Validator precompile entry point ──────────────────────────────────

/// Register this precompile function with `call-precompiles`.
///
/// Call once during node startup (e.g. in `CallNode::new`).
pub fn register_validator_precompile() {
    let _ = set_validator_precompile_fn(validator_precompile_fn);
}

/// Register the oracle validator checker with `call-precompiles`.
///
/// Call once during node startup. This lets the Oracle precompile reject
/// `submitPrice` calls from non-qualified validators.
pub fn register_oracle_validator_check() {
    let _ = call_precompiles::set_oracle_validator_check(is_current_validator);
}

/// Check whether `addr` is a qualified (current-epoch) validator.
/// Reads from the thread-local validator state injected by `ValidatorStateHookGuard`.
fn is_current_validator(addr: &Address) -> bool {
    with_validator_state(|vs, _current_block| vs.is_qualified_validator(addr))
        .unwrap_or(false)
}

pub fn validator_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0x8c, 0xaa, 0x52, 0x30] => stake(input, gas_limit),
        &[0x80, 0x9e, 0xe5, 0x7d] => unstake(input, gas_limit),
        &[0xe8, 0xf5, 0x90, 0x72] => claim_unbonded(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── stake(bytes32 ed25519Pubkey, uint256 selfStake) -> uint32 ─────────

fn stake(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 30000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&input[4..36]);
    let self_stake = decode_u256(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid self_stake".into())
    })?;

    let caller = require_caller()?;

    // Verify sender has sufficient balance
    let has_balance = with_account_state(|acc| {
        acc.get_balance(call_protocol::CALL_ASSET_ID, &caller) >= self_stake
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))?;

    if !has_balance {
        return Err(PrecompileError::Other("insufficient balance".into()));
    }

    // Transfer stake to escrow
    with_account_state(|acc| {
        acc.transfer(
            call_protocol::CALL_ASSET_ID,
            caller,
            STAKING_ESCROW,
            self_stake,
        )
        .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    // Register validator
    let validator_id = with_validator_state(|vs, current_block| {
        vs.set_current_block(current_block);
        vs.stake(caller, pubkey, self_stake)
            .map_err(|e| PrecompileError::Other(format!("stake: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("validator state not available".into()))
    .and_then(|r| r)?;

    // Encode uint32 as uint256 (32 bytes)
    let mut output = [0u8; 32];
    output[28..32].copy_from_slice(&validator_id.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── unstake(uint32 validatorId) ───────────────────────────────────────

fn unstake(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let validator_id = decode_u32(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid validator_id".into())
    })?;

    let caller = require_caller()?;

    with_validator_state(|vs, current_block| {
        vs.set_current_block(current_block);
        vs.unstake(validator_id, caller)
            .map_err(|e| PrecompileError::Other(format!("unstake: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("validator state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── claimUnbonded(uint32 validatorId) -> uint256 amount ───────────────

fn claim_unbonded(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 15000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let validator_id = decode_u32(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid validator_id".into())
    })?;

    let (amount, recipient) = with_validator_state(|vs, current_block| {
        vs.set_current_block(current_block);
        vs.claim_unbonded(validator_id)
            .map_err(|e| PrecompileError::Other(format!("claimUnbonded: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("validator state not available".into()))
    .and_then(|r| r)?;

    // Return staked tokens from escrow to the original staker
    with_account_state(|acc| {
        acc.transfer(
            call_protocol::CALL_ASSET_ID,
            STAKING_ESCROW,
            recipient,
            amount,
        )
        .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    // Encode uint256 amount (32 bytes)
    let mut output = [0u8; 32];
    output[16..32].copy_from_slice(&amount.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::from(output.to_vec()),
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
    use crate::validator::ValidatorStateManager;

    fn setup_state_hook(
        account: &mut AccountState,
        validator_state: &mut ValidatorStateManager,
        current_block: u64,
    ) -> (call_precompiles::state_hook::StateHookGuard, ValidatorStateHookGuard) {
        let mut registry = call_protocol::registry::AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::default();
        let mut shielded = call_shielded::ShieldedState::new();
        let mut oracle = call_oracle::OracleManager::default();
        let mut gov = call_governance::GovernanceManager::default();

        let state_guard = unsafe {
            call_precompiles::state_hook::StateHookGuard::from_raw(
                account,
                &mut registry,
                &mut compliance,
                &mut shielded,
                &mut oracle,
                &mut gov,
            )
        };
        let validator_guard = ValidatorStateHookGuard::new(validator_state, current_block);
        (state_guard, validator_guard)
    }

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    #[test]
    fn test_stake() {
        let mut account = AccountState::new();
        let mut validator_state = ValidatorStateManager::new();
        let caller = test_addr(1);
        let stake_amount = validator_state.params.min_self_stake;

        // Fund caller
        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, stake_amount)
            .unwrap();

        let (_sg, _vg) = setup_state_hook(&mut account, &mut validator_state, 100);
        call_precompiles::set_current_caller(Some(caller));

        // Encode: stake(pubkey, selfStake)
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x8c, 0xaa, 0x52, 0x30]);
        input[4..36].copy_from_slice(&test_pubkey(1));
        input[52..68].copy_from_slice(&stake_amount.to_be_bytes());

        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "stake failed: {:?}", result);

        // Verify validator was created
        let output = result.unwrap().bytes;
        let validator_id = u32::from_be_bytes({
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&output[28..32]);
            buf
        });
        assert_eq!(validator_id, 0);

        // Verify balance was transferred to escrow
        let escrow_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &STAKING_ESCROW);
        assert_eq!(escrow_balance, stake_amount);

        // Verify caller balance is zero
        let caller_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &caller);
        assert_eq!(caller_balance, 0);

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_stake_insufficient_balance() {
        let mut account = AccountState::new();
        let mut validator_state = ValidatorStateManager::new();
        let caller = test_addr(1);
        let stake_amount = validator_state.params.min_self_stake;

        // Do NOT fund caller
        let (_sg, _vg) = setup_state_hook(&mut account, &mut validator_state, 100);
        call_precompiles::set_current_caller(Some(caller));

        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x8c, 0xaa, 0x52, 0x30]);
        input[4..36].copy_from_slice(&test_pubkey(1));
        input[52..68].copy_from_slice(&stake_amount.to_be_bytes());

        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_err());

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_unstake_and_claim() {
        let mut account = AccountState::new();
        let mut validator_state = ValidatorStateManager::new();
        let caller = test_addr(1);
        let stake_amount = validator_state.params.min_self_stake;

        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, stake_amount)
            .unwrap();

        let (_sg, _vg) = setup_state_hook(&mut account, &mut validator_state, 100);
        call_precompiles::set_current_caller(Some(caller));

        // Stake first
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x8c, 0xaa, 0x52, 0x30]);
        input[4..36].copy_from_slice(&test_pubkey(1));
        input[52..68].copy_from_slice(&stake_amount.to_be_bytes());
        let result = validator_precompile_fn(&input, 50000).unwrap();
        let validator_id = u32::from_be_bytes({
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&result.bytes[28..32]);
            buf
        });

        // Unstake
        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0x80, 0x9e, 0xe5, 0x7d]);
        input[28..32].copy_from_slice(&validator_id.to_be_bytes());
        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "unstake failed: {:?}", result);

        // Claim before unbonding period — should fail
        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0xe8, 0xf5, 0x90, 0x72]);
        input[28..32].copy_from_slice(&validator_id.to_be_bytes());
        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_err(), "claim should fail before unbonding period");

        // Advance past unbonding period by creating new guard with higher block
        drop(_sg);
        drop(_vg);
        let advanced_block = 100 + validator_state.params.unbonding_period_blocks + 1;
        let (_sg2, _vg2) = setup_state_hook(&mut account, &mut validator_state, advanced_block);
        call_precompiles::set_current_caller(Some(caller));

        // Claim now succeeds
        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "claim failed: {:?}", result);

        // Verify tokens returned to caller
        let caller_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &caller);
        assert_eq!(caller_balance, stake_amount);

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_unstake_not_owner() {
        let mut account = AccountState::new();
        let mut validator_state = ValidatorStateManager::new();
        let caller = test_addr(1);
        let stake_amount = validator_state.params.min_self_stake;

        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, stake_amount)
            .unwrap();

        let (_sg, _vg) = setup_state_hook(&mut account, &mut validator_state, 100);
        call_precompiles::set_current_caller(Some(caller));

        // Stake
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x8c, 0xaa, 0x52, 0x30]);
        input[4..36].copy_from_slice(&test_pubkey(1));
        input[52..68].copy_from_slice(&stake_amount.to_be_bytes());
        let result = validator_precompile_fn(&input, 50000).unwrap();
        let validator_id = u32::from_be_bytes({
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&result.bytes[28..32]);
            buf
        });

        // Different caller tries to unstake
        call_precompiles::set_current_caller(Some(test_addr(2)));

        let mut input = vec![0u8; 36];
        input[0..4].copy_from_slice(&[0x80, 0x9e, 0xe5, 0x7d]);
        input[28..32].copy_from_slice(&validator_id.to_be_bytes());
        let result = validator_precompile_fn(&input, 50000);
        assert!(result.is_err());

        call_precompiles::set_current_caller(None);
    }
}

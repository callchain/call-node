//! Agent precompile implementation for `call-agent`.
//!
//! Precompile at address `0x209` providing:
//! - register(bytes pubkey, string name, string url)
//! - grant(uint64 agentId, uint64 assetId, uint256 amount)
//! - revoke(uint64 agentId, uint64 assetId)

use std::cell::RefCell;

use call_precompiles::state_hook::with_account_state;
use call_precompiles::{current_caller, PrecompileError, PrecompileOutput, PrecompileResult};
use call_primitives::Address;

use crate::{AgentBalances, AgentRegistry};

// ── Thread-local agent state reference ────────────────────────────────

thread_local! {
    static TL_AGENT_STATE: RefCell<Option<AgentStateRef>> = RefCell::new(None);
}

#[derive(Clone, Copy)]
struct AgentStateRef {
    agent_registry: *mut AgentRegistry,
    agent_balances: *mut AgentBalances,
    base_fee: u128,
    current_block: u64,
}

// Safety: AgentStateRef is !Send and !Sync because it contains raw
// pointers, and we only use it via thread-local storage.

/// Guard that injects agent state into thread-local storage for precompiles.
pub struct AgentStateHookGuard;

impl AgentStateHookGuard {
    /// Inject agent state into the precompile hook layer.
    ///
    /// # Safety
    /// The caller must ensure that all passed references outlive this guard.
    pub fn new(
        agent_registry: &mut AgentRegistry,
        agent_balances: &mut AgentBalances,
        base_fee: u128,
        current_block: u64,
    ) -> Self {
        let refs = AgentStateRef {
            agent_registry,
            agent_balances,
            base_fee,
            current_block,
        };
        TL_AGENT_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }
}

impl Drop for AgentStateHookGuard {
    fn drop(&mut self) {
        TL_AGENT_STATE.with(|t| *t.borrow_mut() = None);
    }
}

/// Access the agent state (if hooked).
fn with_agent_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AgentRegistry, &mut AgentBalances, u128, u64) -> R,
{
    TL_AGENT_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| unsafe {
            f(
                &mut *r.agent_registry,
                &mut *r.agent_balances,
                r.base_fee,
                r.current_block,
            )
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

fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

/// Decode a dynamic `string` or `bytes` type from ABI input.
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

fn decode_string(input: &[u8], slot_offset: usize) -> Option<String> {
    let bytes = decode_bytes(input, slot_offset)?;
    String::from_utf8(bytes).ok()
}

// ── Agent precompile entry point ──────────────────────────────────────

/// No-op: agent precompile is now stateful and self-contained.
#[deprecated(note = "agent precompile is stateful; registration no longer needed")]
pub fn register_agent_precompile() {}

pub fn agent_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0x4d, 0x43, 0x1f, 0x19] => register(input, gas_limit),
        &[0x90, 0xd9, 0xbc, 0xca] => grant(input, gas_limit),
        &[0x75, 0xde, 0xa5, 0x39] => revoke(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── register(bytes pubkey, string name, string url) ───────────────────

fn register(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let pubkey_bytes = decode_bytes(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid pubkey".into())
    })?;
    if pubkey_bytes.len() != 64 {
        return Err(PrecompileError::Other("pubkey must be 64 bytes".into()));
    }
    let mut pubkey = [0u8; 64];
    pubkey.copy_from_slice(&pubkey_bytes);
    let name = decode_string(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid name".into())
    })?;
    let url = decode_string(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid url".into())
    })?;

    let caller = require_caller()?;

    let agent_id = with_agent_state(|registry, _balances, base_fee, current_block| {
        // Deduct registration fee if configured (same pattern as instruction executor)
        if base_fee > 0 {
            let deducted = with_account_state(|acc| {
                acc.deduct_balance(call_protocol::CALL_ASSET_ID, caller, base_fee)
                    .is_ok()
            });
            if !deducted.unwrap_or(false) {
                return Err(PrecompileError::Other(
                    "insufficient balance for registration fee".into(),
                ));
            }
        }

        registry
            .register_agent(
                caller,
                pubkey,
                name,
                url,
                [0u8; 32],
                None,
                current_block,
                None,
            )
            .map_err(|e| PrecompileError::Other(format!("register: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("agent state not available".into()))
    .and_then(|r| r)?;

    // Return agent_id as uint64 (in uint256 slot)
    let mut output = [0u8; 32];
    output[24..32].copy_from_slice(&agent_id.to_be_bytes());

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── grant(uint64 agentId, uint64 assetId, uint256 amount) ─────────────

fn grant(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let agent_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid agent_id".into())
    })?;
    let asset_id = decode_u64(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let amount = decode_u128(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let caller = require_caller()?;

    with_agent_state(|registry, balances, _base_fee, _current_block| {
        let agent = registry.get_agent(agent_id).ok_or_else(|| {
            PrecompileError::Other("agent not found".into())
        })?;
        if agent.owner != caller {
            return Err(PrecompileError::Other(
                "only agent owner can grant".into(),
            ));
        }
        let account = with_account_state(|acc| acc as *mut call_protocol::AccountState)
            .ok_or_else(|| PrecompileError::Other("account state not available".into()))?;
        // SAFETY: The pointer is valid because StateHookGuard is alive
        let acc_ref = unsafe { &mut *account };
        balances
            .grant_funds(caller, agent_id, asset_id, amount, acc_ref)
            .map_err(|e| PrecompileError::Other(format!("grant: {e}").into()))
    })
    .ok_or_else(|| PrecompileError::Other("agent state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: call_precompiles::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── revoke(uint64 agentId, uint64 assetId) ────────────────────────────

fn revoke(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 8000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let agent_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid agent_id".into())
    })?;
    let asset_id = decode_u64(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;

    let caller = require_caller()?;

    with_agent_state(|registry, balances, _base_fee, _current_block| {
        let agent = registry.get_agent(agent_id).ok_or_else(|| {
            PrecompileError::Other("agent not found".into())
        })?;
        if agent.owner != caller {
            return Err(PrecompileError::Other(
                "only agent owner can revoke".into(),
            ));
        }
        balances.revoke_funds(caller, agent_id, asset_id);
        Ok(())
    })
    .ok_or_else(|| PrecompileError::Other("agent state not available".into()))
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
        registry: &mut AgentRegistry,
        balances: &mut AgentBalances,
        base_fee: u128,
        current_block: u64,
    ) -> (call_precompiles::state_hook::StateHookGuard, AgentStateHookGuard) {
        let mut asset_registry = call_protocol::registry::AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::default();
        let mut shielded = call_shielded::ShieldedState::new();

        let state_guard = unsafe {
            call_precompiles::state_hook::StateHookGuard::from_raw(
                account,
                &mut asset_registry,
                &mut compliance,
                &mut shielded,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        let agent_guard = AgentStateHookGuard::new(registry, balances, base_fee, current_block);
        (state_guard, agent_guard)
    }

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn encode_register_agent(pubkey: [u8; 64], name: &str, url: &str) -> Vec<u8> {
        let name_bytes = name.as_bytes();
        let url_bytes = url.as_bytes();

        // Calculate offsets (relative to args start = byte 4)
        let pubkey_offset = 96usize; // 3 slots after selector
        let name_offset = pubkey_offset + 32 + ((pubkey.len() + 31) / 32) * 32;
        let url_offset = name_offset + 32 + ((name_bytes.len() + 31) / 32) * 32;

        let total_size = 4
            + 96
            + 32 + ((pubkey.len() + 31) / 32) * 32
            + 32 + ((name_bytes.len() + 31) / 32) * 32
            + 32 + ((url_bytes.len() + 31) / 32) * 32;

        let mut input = vec![0u8; total_size];

        // Selector
        input[0..4].copy_from_slice(&[0x4d, 0x43, 0x1f, 0x19]);
        // Pubkey offset (uint256, last 8 bytes of slot at byte 4)
        let pubkey_offset_u64 = pubkey_offset as u64;
        input[28..36].copy_from_slice(&pubkey_offset_u64.to_be_bytes());
        // Name offset (uint256, last 8 bytes of slot at byte 36)
        let name_offset_u64 = name_offset as u64;
        input[60..68].copy_from_slice(&name_offset_u64.to_be_bytes());
        // URL offset (uint256, last 8 bytes of slot at byte 68)
        let url_offset_u64 = url_offset as u64;
        input[92..100].copy_from_slice(&url_offset_u64.to_be_bytes());

        // Pubkey: length + data
        let pubkey_start = 4 + pubkey_offset;
        input[pubkey_start + 24..pubkey_start + 32].copy_from_slice(&pubkey.len().to_be_bytes());
        input[pubkey_start + 32..pubkey_start + 32 + pubkey.len()].copy_from_slice(&pubkey);

        // Name: length + data
        let name_start = 4 + name_offset;
        input[name_start + 24..name_start + 32].copy_from_slice(&name_bytes.len().to_be_bytes());
        input[name_start + 32..name_start + 32 + name_bytes.len()].copy_from_slice(name_bytes);

        // URL: length + data
        let url_start = 4 + url_offset;
        input[url_start + 24..url_start + 32].copy_from_slice(&url_bytes.len().to_be_bytes());
        input[url_start + 32..url_start + 32 + url_bytes.len()].copy_from_slice(url_bytes);

        input
    }

    #[test]
    fn test_register_agent() {
        let mut account = AccountState::new();
        let mut registry = AgentRegistry::new_with_format_verifier();
        let mut balances = AgentBalances::new();
        let caller = test_addr(1);

        // Fund caller for fee
        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, 10000)
            .unwrap();

        let (_sg, _ag) = setup_state_hook(&mut account, &mut registry, &mut balances, 100, 1);
        call_precompiles::set_current_caller(Some(caller));

        let input = encode_register_agent([0u8; 64], "TestAgent", "http://test.com");
        let result = agent_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "register failed: {:?}", result);

        // Verify agent was registered
        let agent = registry.get_agent(0).unwrap();
        assert_eq!(agent.owner, caller);
        assert_eq!(agent.name, "TestAgent");
        assert_eq!(agent.url, "http://test.com");

        // Fee was deducted
        let caller_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &caller);
        assert_eq!(caller_balance, 10000 - 100);

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_grant_and_revoke_agent_balance() {
        let mut account = AccountState::new();
        let mut registry = AgentRegistry::new_with_format_verifier();
        let mut balances = AgentBalances::new();
        let caller = test_addr(1);

        // Fund caller
        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, 10000)
            .unwrap();
        account
            .credit_balance(2, caller, 5000)
            .unwrap();

        let (_sg, _ag) = setup_state_hook(&mut account, &mut registry, &mut balances, 0, 1);
        call_precompiles::set_current_caller(Some(caller));

        // Register agent first
        let input = encode_register_agent([0u8; 64], "TestAgent", "http://test.com");
        let result = agent_precompile_fn(&input, 50000);
        assert!(result.is_ok());
        let agent_id = 0u64;

        // Grant balance
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0x90, 0xd9, 0xbc, 0xca]);
        input[28..36].copy_from_slice(&agent_id.to_be_bytes());
        input[60..68].copy_from_slice(&2u64.to_be_bytes());
        input[84..100].copy_from_slice(&1000u128.to_be_bytes());

        let result = agent_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "grant failed: {:?}", result);

        // Verify agent balance
        assert_eq!(balances.get_balance(caller, agent_id, 2), 1000);

        // Revoke balance
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x75, 0xde, 0xa5, 0x39]);
        input[28..36].copy_from_slice(&agent_id.to_be_bytes());
        input[60..68].copy_from_slice(&2u64.to_be_bytes());

        let result = agent_precompile_fn(&input, 50000);
        assert!(result.is_ok(), "revoke failed: {:?}", result);

        // Verify agent balance is zero
        assert_eq!(balances.get_balance(caller, agent_id, 2), 0);

        call_precompiles::set_current_caller(None);
    }

    #[test]
    fn test_grant_not_owner() {
        let mut account = AccountState::new();
        let mut registry = AgentRegistry::new_with_format_verifier();
        let mut balances = AgentBalances::new();
        let caller = test_addr(1);

        account
            .credit_balance(call_protocol::CALL_ASSET_ID, caller, 10000)
            .unwrap();

        let (_sg, _ag) = setup_state_hook(&mut account, &mut registry, &mut balances, 0, 1);
        call_precompiles::set_current_caller(Some(caller));

        // Register agent
        let input = encode_register_agent([0u8; 64], "TestAgent", "http://test.com");
        agent_precompile_fn(&input, 50000).unwrap();

        // Different caller tries to grant
        call_precompiles::set_current_caller(Some(test_addr(2)));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0x90, 0xd9, 0xbc, 0xca]);
        input[28..36].copy_from_slice(&0u64.to_be_bytes());
        input[60..68].copy_from_slice(&2u64.to_be_bytes());
        input[84..100].copy_from_slice(&100u128.to_be_bytes());

        let result = agent_precompile_fn(&input, 50000);
        assert!(result.is_err());

        call_precompiles::set_current_caller(None);
    }
}

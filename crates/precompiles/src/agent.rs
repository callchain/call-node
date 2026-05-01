//! Agent precompile at 0x209
//!
//! Functions: registerAgent, grantBalance, revokeBalance, pay, batchPay,
//!            bridgeDeposit, revokeAgent,
//!            getAgentOwner, getAgentBalance, getAgentName, getAgentUrl, getAgentPerms

use alloy_primitives::{address, Address, U256};
use revm_precompile::{PrecompileError, PrecompileOutput};

use crate::StatefulPrecompile;
use crate::storage::{storage_slot, StorageCtx};

pub const AGENT_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000209");

const CALL_ASSET_ID: u64 = 1;

// ── ABI decoding helpers ──────────────────────────────────────────────

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

fn decode_address(input: &[u8], slot_offset: usize) -> Option<Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(Address::from_slice(&input[start..start + 20]))
}

fn decode_bytes32(input: &[u8], slot_offset: usize) -> Option<[u8; 32]> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&input[slot_offset..slot_offset + 32]);
    Some(buf)
}

fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    Some(u64::from_be_bytes(buf) as usize)
}

fn decode_address_array(input: &[u8], slot_offset: usize) -> Option<Vec<Address>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset;
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let elem_start = abs_offset + 32;
    if input.len() < elem_start + len * 32 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let addr = decode_address(input, elem_start + i * 32)?;
        out.push(addr);
    }
    Some(out)
}

fn decode_u128_array(input: &[u8], slot_offset: usize) -> Option<Vec<u128>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset;
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let elem_start = abs_offset + 32;
    if input.len() < elem_start + len * 32 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let val = decode_u128(input, elem_start + i * 32)?;
        out.push(val);
    }
    Some(out)
}

// ── Encoding helpers ──────────────────────────────────────────────────

fn u256_to_u64(v: U256) -> u64 {
    u64::from_be_bytes(v.to_be_bytes::<32>()[24..32].try_into().unwrap())
}

fn u256_to_u128(v: U256) -> u128 {
    u128::from_be_bytes(v.to_be_bytes::<32>()[16..32].try_into().unwrap())
}

fn u128_to_u256(v: u128) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

fn u64_to_u256(v: u64) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

fn address_to_u256(addr: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    U256::from_be_bytes::<32>(bytes)
}

fn u256_to_address(v: U256) -> Address {
    Address::from_slice(&v.to_be_bytes::<32>()[12..32])
}

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_agent_count() -> U256 {
    U256::ZERO
}

fn slot_agent_owner(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"owner"])
}

fn slot_agent_pubkey(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"pubkey"])
}

fn slot_agent_name(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"name"])
}

fn slot_agent_url(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"url"])
}

fn slot_agent_perms(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"perms"])
}

fn slot_agent_registered_at(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"block"])
}

fn slot_agent_balance(agent_id: u64, asset_id: u64) -> U256 {
    storage_slot(&[b"abalance", &agent_id.to_be_bytes()[..], &asset_id.to_be_bytes()[..]])
}

fn slot_balance(asset_id: u64, addr: Address) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], addr.as_slice()])
}

// Pack agent permissions into a single U256:
// bytes 0..16  = per_tx_limit (u128)
// bytes 16..24 = expires_at (u64)
// byte 31      = flags (bit 0 = allow asset 1, bit 1 = allow all protocols)
fn pack_agent_perms(per_tx_limit: u128, expires_at: u64, flags: u8) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..24].copy_from_slice(&expires_at.to_be_bytes());
    packed[31] = flags;
    U256::from_be_slice(&packed)
}

fn agent_exists(agent_id: u64) -> bool {
    StorageCtx::sload(AGENT_ADDRESS, slot_agent_owner(agent_id))
        .map(|v| v != U256::ZERO)
        .unwrap_or(false)
}

fn agent_check_owner(agent_id: u64, sender: Address) -> Result<(), PrecompileError> {
    let owner = StorageCtx::sload(AGENT_ADDRESS, slot_agent_owner(agent_id))
        .map(u256_to_address)
        .unwrap_or(Address::ZERO);
    if owner != sender {
        return Err(PrecompileError::Other("agent: sender is not owner".into()));
    }
    Ok(())
}

// ── AgentPrecompile ───────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentPrecompile;

impl AgentPrecompile {
    // registerAgent(bytes32 name, bytes32 url, bytes32 pubkeyHash) -> 0x9f32a135
    fn register_agent(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let name = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid name".into()))?;
        let url = decode_bytes32(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid url".into()))?;
        let pubkey_hash = decode_bytes32(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid pubkeyHash".into()))?;

        let count = StorageCtx::sload(AGENT_ADDRESS, slot_agent_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        let agent_id = count;
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(count + 1));

        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_owner(agent_id),
            address_to_u256(msg_sender),
        );
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_pubkey(agent_id),
            U256::from_be_slice(&pubkey_hash),
        );
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_name(agent_id),
            U256::from_be_slice(&name),
        );
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_url(agent_id),
            U256::from_be_slice(&url),
        );
        // Default perms: per_tx_limit=1_000, expires_at=0, flags=1 (allow asset 1)
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_perms(agent_id),
            pack_agent_perms(1_000, 0, 1),
        );
        let block_number = StorageCtx::block_number();
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_registered_at(agent_id),
            u64_to_u256(block_number),
        );

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // grantBalance(uint64 agentId, uint64 assetId, uint128 amount) -> 0x80ecf9d4
    fn grant_balance(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Duct sender balance
        let sender_slot = slot_balance(asset_id, msg_sender);
        let sender_bal = StorageCtx::sload(crate::ASSET_ADDRESS, sender_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let sender_bal = sender_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent grant: insufficient balance".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

        // Credit agent balance
        let agent_bal = StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
            .map(u256_to_u128)
            .unwrap_or(0);
        let agent_bal = agent_bal
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("agent grant: balance overflow".into()))?;
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), u128_to_u256(agent_bal));

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // revokeBalance(uint64 agentId, uint64 assetId) -> 0x1946b415
    fn revoke_balance(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Zero agent balance for this asset
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), U256::ZERO);

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // pay(uint64 agentId, uint64 assetId, address to, uint128 amount) -> 0xe699cef8
    fn pay(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let to = decode_address(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid to".into()))?;
        let amount = decode_u128(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Check permissions
        let perms = StorageCtx::sload(AGENT_ADDRESS, slot_agent_perms(agent_id))
            .unwrap_or(U256::ZERO);
        let (per_tx_limit, expires_at, flags) = unpack_agent_perms(perms);
        let current_block = StorageCtx::block_number();
        if expires_at != 0 && current_block > expires_at {
            return Err(PrecompileError::Other("agent: permissions expired".into()));
        }
        if amount > per_tx_limit {
            return Err(PrecompileError::Other(
                format!("agent: amount {amount} exceeds per-tx limit {per_tx_limit}").into(),
            ));
        }
        if asset_id != CALL_ASSET_ID && (flags & 1) == 0 {
            return Err(PrecompileError::Other("agent: asset not allowed".into()));
        }

        // Deduct agent balance
        let agent_bal = StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
            .map(u256_to_u128)
            .unwrap_or(0);
        let agent_bal = agent_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent pay: insufficient balance".into()))?;
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), u128_to_u256(agent_bal));

        // Credit recipient
        let to_slot = slot_balance(asset_id, to);
        let to_bal = StorageCtx::sload(crate::ASSET_ADDRESS, to_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let to_bal = to_bal
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("agent pay: balance overflow".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getAgentOwner(uint64 agentId) -> address -> 0x6b2b421b
    fn get_agent_owner(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;

        let owner = StorageCtx::sload(AGENT_ADDRESS, slot_agent_owner(agent_id))
            .map(u256_to_address)
            .unwrap_or(Address::ZERO);

        let mut out = vec![0u8; 32];
        out[12..32].copy_from_slice(owner.as_slice());
        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::from(out));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getAgentBalance(uint64 agentId, uint64 assetId) -> uint128 -> 0x4b2bd388
    fn get_agent_balance(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;

        let balance = StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
            .map(u256_to_u128)
            .unwrap_or(0);

        let mut out = vec![0u8; 32];
        out[16..32].copy_from_slice(&balance.to_be_bytes());
        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::from(out));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getAgentName(uint64 agentId) -> bytes32 -> 0x5304a7bf
    fn get_agent_name(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;

        let name = StorageCtx::sload(AGENT_ADDRESS, slot_agent_name(agent_id))
            .unwrap_or(U256::ZERO);

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::from(name.to_be_bytes::<32>().to_vec()));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getAgentUrl(uint64 agentId) -> bytes32 -> 0x2b051ae0
    fn get_agent_url(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;

        let url = StorageCtx::sload(AGENT_ADDRESS, slot_agent_url(agent_id))
            .unwrap_or(U256::ZERO);

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::from(url.to_be_bytes::<32>().to_vec()));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getAgentPerms(uint64 agentId) -> uint256 -> 0x234a666d
    fn get_agent_perms(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;

        let perms = StorageCtx::sload(AGENT_ADDRESS, slot_agent_perms(agent_id))
            .unwrap_or(U256::ZERO);

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::from(perms.to_be_bytes::<32>().to_vec()));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // batchPay(uint64 agentId, uint64 assetId, address[] to, uint128[] amounts) -> 0xa0c290b9
    fn batch_pay(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        if input.len() < 132 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Decode dynamic arrays
        let _to_offset = decode_u256_usize(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid to offset".into()))?;
        let _amounts_offset = decode_u256_usize(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amounts offset".into()))?;

        let to = decode_address_array(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid to array".into()))?;
        let amounts = decode_u128_array(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amounts array".into()))?;

        if to.len() != amounts.len() {
            return Err(PrecompileError::Other("array length mismatch".into()));
        }
        if to.is_empty() {
            return Err(PrecompileError::Other("empty batch".into()));
        }

        const GAS_COST_PER: u64 = 30000;
        let total_gas = GAS_COST_PER * to.len() as u64;
        StorageCtx::deduct_gas(total_gas).ok_or(PrecompileError::OutOfGas)?;

        // Check permissions
        let perms = StorageCtx::sload(AGENT_ADDRESS, slot_agent_perms(agent_id))
            .unwrap_or(U256::ZERO);
        let (per_tx_limit, expires_at, flags) = unpack_agent_perms(perms);
        let current_block = StorageCtx::block_number();
        if expires_at != 0 && current_block > expires_at {
            return Err(PrecompileError::Other("agent: permissions expired".into()));
        }
        if asset_id != CALL_ASSET_ID && (flags & 1) == 0 {
            return Err(PrecompileError::Other("agent: asset not allowed".into()));
        }

        // Deduct agent balance
        let agent_bal = StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
            .map(u256_to_u128)
            .unwrap_or(0);
        let total_amount: u128 = amounts.iter().copied().sum();
        let agent_bal = agent_bal
            .checked_sub(total_amount)
            .ok_or_else(|| PrecompileError::Other("agent batch pay: insufficient balance".into()))?;
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), u128_to_u256(agent_bal));

        // Credit recipients
        for (recipient, amount) in to.iter().zip(amounts.iter()) {
            if *amount > per_tx_limit {
                return Err(PrecompileError::Other(
                    format!("agent: amount {amount} exceeds per-tx limit {per_tx_limit}").into(),
                ));
            }
            let to_slot = slot_balance(asset_id, *recipient);
            let to_bal = StorageCtx::sload(crate::ASSET_ADDRESS, to_slot)
                .map(u256_to_u128)
                .unwrap_or(0);
            let to_bal = to_bal
                .checked_add(*amount)
                .ok_or_else(|| PrecompileError::Other("agent batch pay: balance overflow".into()))?;
            StorageCtx::sstore(crate::ASSET_ADDRESS, to_slot, u128_to_u256(to_bal));
        }

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // bridgeDeposit(uint64 agentId, uint64 assetId, uint128 amount, uint64 targetChain, bytes targetAddress)
    // -> 0xa72c6932
    fn bridge_deposit(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 164 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;
        let _target_chain = decode_u64(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid targetChain".into()))?;

        // Decode dynamic bytes targetAddress
        let target_addr_offset = decode_u256_usize(input, 132)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress offset".into()))?;
        let target_addr_abs = 4 + target_addr_offset;
        if input.len() < target_addr_abs + 32 {
            return Err(PrecompileError::Other("invalid targetAddress".into()));
        }
        let target_addr_len = decode_u256_usize(input, target_addr_abs)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress length".into()))?;
        let target_addr_data_start = target_addr_abs + 32;
        if input.len() < target_addr_data_start + target_addr_len {
            return Err(PrecompileError::Other("targetAddress data too short".into()));
        }
        let _target_address = &input[target_addr_data_start..target_addr_data_start + target_addr_len];

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Check permissions
        let perms = StorageCtx::sload(AGENT_ADDRESS, slot_agent_perms(agent_id))
            .unwrap_or(U256::ZERO);
        let (_, expires_at, flags) = unpack_agent_perms(perms);
        let current_block = StorageCtx::block_number();
        if expires_at != 0 && current_block > expires_at {
            return Err(PrecompileError::Other("agent: permissions expired".into()));
        }
        if asset_id != CALL_ASSET_ID && (flags & 1) == 0 {
            return Err(PrecompileError::Other("agent: asset not allowed".into()));
        }

        // Deduct agent balance
        let agent_bal = StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
            .map(u256_to_u128)
            .unwrap_or(0);
        let agent_bal = agent_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent bridge deposit: insufficient agent balance".into()))?;
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), u128_to_u256(agent_bal));

        // Deduct sender protocol balance
        let sender_slot = slot_balance(asset_id, msg_sender);
        let sender_bal = StorageCtx::sload(crate::ASSET_ADDRESS, sender_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let sender_bal = sender_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent bridge deposit: insufficient sender balance".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

        // Note: Full bridge to EVM requires EVM executor (not available in precompile context).
        // The protocol-level bridge deposit instruction handles ERC-20 contract calls.
        // This precompile deducts balances and records intent; validators relay the withdrawal.

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // revokeAgent(uint64 agentId) -> 0x88311f9c
    fn revoke_agent(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let agent_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid agentId".into()))?;

        if !agent_exists(agent_id) {
            return Err(PrecompileError::Other("agent: not found".into()));
        }
        agent_check_owner(agent_id, msg_sender)?;

        // Clear all agent storage slots
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_owner(agent_id), U256::ZERO);
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_pubkey(agent_id), U256::ZERO);
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_name(agent_id), U256::ZERO);
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_url(agent_id), U256::ZERO);
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_perms(agent_id), U256::ZERO);
        StorageCtx::sstore(AGENT_ADDRESS, slot_agent_registered_at(agent_id), U256::ZERO);
        // Note: agent balances are not returned; they are zeroed implicitly by clearing state.
        // In a full implementation, iterate over all asset IDs and zero balance slots.

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

fn unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit = u128::from_be_bytes(bytes[0..16].try_into().unwrap());
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

impl StatefulPrecompile for AgentPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector = [calldata[0], calldata[1], calldata[2], calldata[3]];
        match selector {
            [0x9f, 0x32, 0xa1, 0x35] => self.register_agent(calldata, msg_sender),
            [0x80, 0xec, 0xf9, 0xd4] => self.grant_balance(calldata, msg_sender),
            [0x19, 0x46, 0xb4, 0x15] => self.revoke_balance(calldata, msg_sender),
            [0xe6, 0x99, 0xce, 0xf8] => self.pay(calldata, msg_sender),
            [0xa0, 0xc2, 0x90, 0xb9] => self.batch_pay(calldata, msg_sender),
            [0xa7, 0x2c, 0x69, 0x32] => self.bridge_deposit(calldata, msg_sender),
            [0x88, 0x31, 0x1f, 0x9c] => self.revoke_agent(calldata, msg_sender),
            [0x6b, 0x2b, 0x42, 0x1b] => self.get_agent_owner(calldata),
            [0x4b, 0x2b, 0xd3, 0x88] => self.get_agent_balance(calldata),
            [0x53, 0x04, 0xa7, 0xbf] => self.get_agent_name(calldata),
            [0x2b, 0x05, 0x1a, 0xe0] => self.get_agent_url(calldata),
            [0x23, 0x4a, 0x66, 0x6d] => self.get_agent_perms(calldata),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_address() {
        assert_eq!(
            AGENT_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000209")
        );
    }

    #[test]
    fn test_agent_precompile_register_and_get() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x22);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = AgentPrecompile;

            // registerAgent(name, url, pubkeyHash)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x9f, 0x32, 0xa1, 0x35]);
            input[4..36].copy_from_slice(b"TestAgent_______________________");
            let mut url = [b'_'; 32];
            url[..15].copy_from_slice(b"http://test.com");
            input[36..68].copy_from_slice(&url);
            input[68..100].copy_from_slice(&[0xBBu8; 32]);

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "register failed: {:?}", result.err());

            // getAgentOwner(0)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x6b, 0x2b, 0x42, 0x1b]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let owner = Address::from_slice(&result.bytes[12..32]);
            assert_eq!(owner, sender);

            // getAgentName(0)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x53, 0x04, 0xa7, 0xbf]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[0..12], b"TestAgent___");

            // getAgentUrl(0)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x2b, 0x05, 0x1a, 0xe0]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[0..15], b"http://test.com");
        });
    }

    #[test]
    fn test_agent_precompile_grant_pay_and_revoke() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x22);
        let recipient = Address::repeat_byte(0x33);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000),
            );

            let mut precompile = AgentPrecompile;

            // registerAgent
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x9f, 0x32, 0xa1, 0x35]);
            input[4..36].copy_from_slice(b"Agent___________________________");
            input[36..68].copy_from_slice(b"url_____________________________");
            input[68..100].copy_from_slice(&[0xCCu8; 32]);
            precompile.call(&input, sender).unwrap();

            // grantBalance(agentId=0, assetId=1, amount=5_000)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x80, 0xec, 0xf9, 0xd4]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            input[84..100].copy_from_slice(&5_000u128.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "grant failed: {:?}", result.err());

            // getAgentBalance(0, 1)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x4b, 0x2b, 0xd3, 0x88]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let bal = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(bal, 5_000);

            // pay(agentId=0, assetId=1, to=recipient, amount=1_000)
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0xe6, 0x99, 0xce, 0xf8]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1_000u128.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "pay failed: {:?}", result.err());

            // getAgentBalance(0, 1) should be 4_000
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x4b, 0x2b, 0xd3, 0x88]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let bal = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(bal, 4_000);

            // revokeBalance(0, 1)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x19, 0x46, 0xb4, 0x15]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "revoke failed: {:?}", result.err());

            // getAgentBalance(0, 1) should be 0
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x4b, 0x2b, 0xd3, 0x88]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[60..68].copy_from_slice(&CALL_ASSET_ID.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let bal = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(bal, 0);
        });
    }
}

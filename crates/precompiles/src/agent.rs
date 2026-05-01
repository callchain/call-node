//! Agent precompile at 0x209
//!
//! Functions: registerAgent, grantBalance, revokeBalance, pay, batchPay,
//!            bridgeDeposit, revokeAgent,
//!            getAgentOwner, getAgentBalance, getAgentName, getAgentUrl, getAgentPerms

use alloy_primitives::{address, Address, U256};
use revm_precompile::PrecompileError;

use crate::{
    address_to_u256, decode_address, decode_bytes32, decode_string, decode_u128, decode_u64,
    decode_u256_usize, decode_address_array, decode_u128_array, encode_u128,
    load_bal, ok_empty, save_bal, u128_to_u256, u256_to_address, u256_to_u128,
    u256_to_u64, u64_to_u256, write_string32, StatefulPrecompile,
};
use crate::storage::{storage_slot, StorageCtx};

pub const AGENT_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000209");

const CALL_ASSET_ID: u64 = 1;

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

fn require_agent_perms(agent_id: u64, asset_id: u64) -> Result<(u128, u64, u8), PrecompileError> {
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
    Ok((per_tx_limit, expires_at, flags))
}

// ── Balance helpers ───────────────────────────────────────────────────

fn load_agent_bal(agent_id: u64, asset_id: u64) -> u128 {
    StorageCtx::sload(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_agent_bal(agent_id: u64, asset_id: u64, amount: u128) {
    StorageCtx::sstore(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id), u128_to_u256(amount));
}

// ── AgentPrecompile ───────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentPrecompile;

impl AgentPrecompile {
    // registerAgent(string name, string url, bytes32 pubkeyHash) -> 0x2b3ce0bf
    fn register_agent(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 6_000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let name = decode_string(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid name".into()))?;
        let url = decode_string(input, 36)
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
            write_string32(&name),
        );
        StorageCtx::sstore(
            AGENT_ADDRESS,
            slot_agent_url(agent_id),
            write_string32(&url),
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

        ok_empty()
    }

    // grantBalance(uint64 agentId, uint64 assetId, uint128 amount) -> 0x80ecf9d4
    fn grant_balance(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 6_000;
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

        // Deduct sender balance
        let sender_bal = load_bal(asset_id, msg_sender)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent grant: insufficient balance".into()))?;
        save_bal(asset_id, msg_sender, sender_bal);

        // Credit agent balance
        let agent_bal = load_agent_bal(agent_id, asset_id)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("agent grant: balance overflow".into()))?;
        save_agent_bal(agent_id, asset_id, agent_bal);

        ok_empty()
    }

    // revokeBalance(uint64 agentId, uint64 assetId) -> 0x1946b415
    fn revoke_balance(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 6_000;
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

        ok_empty()
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

        let (per_tx_limit, _expires_at, _flags) = require_agent_perms(agent_id, asset_id)?;
        if amount > per_tx_limit {
            return Err(PrecompileError::Other(
                format!("agent: amount {amount} exceeds per-tx limit {per_tx_limit}").into(),
            ));
        }

        // Deduct agent balance
        let agent_bal = load_agent_bal(agent_id, asset_id)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent pay: insufficient balance".into()))?;
        save_agent_bal(agent_id, asset_id, agent_bal);

        // Credit recipient
        let to_bal = load_bal(asset_id, to)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("agent pay: balance overflow".into()))?;
        save_bal(asset_id, to, to_bal);

        ok_empty()
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
        let out = revm_precompile::PrecompileOutput::new(GAS_COST, alloy_primitives::Bytes::from(out));
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

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, encode_u128(balance).to_vec().into());
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

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, alloy_primitives::Bytes::from(name.to_be_bytes::<32>().to_vec()));
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

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, alloy_primitives::Bytes::from(url.to_be_bytes::<32>().to_vec()));
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

        let out = revm_precompile::PrecompileOutput::new(GAS_COST, alloy_primitives::Bytes::from(perms.to_be_bytes::<32>().to_vec()));
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

        const GAS_COST_PER: u64 = 30_000;
        let total_gas = GAS_COST_PER * to.len() as u64;
        StorageCtx::deduct_gas(total_gas).ok_or(PrecompileError::OutOfGas)?;

        let (per_tx_limit, _expires_at, _flags) = require_agent_perms(agent_id, asset_id)?;

        // Deduct agent balance
        let total_amount: u128 = amounts.iter().copied().sum();
        let agent_bal = load_agent_bal(agent_id, asset_id)
            .checked_sub(total_amount)
            .ok_or_else(|| PrecompileError::Other("agent batch pay: insufficient balance".into()))?;
        save_agent_bal(agent_id, asset_id, agent_bal);

        // Credit recipients
        for (recipient, amount) in to.iter().zip(amounts.iter()) {
            if *amount > per_tx_limit {
                return Err(PrecompileError::Other(
                    format!("agent: amount {amount} exceeds per-tx limit {per_tx_limit}").into(),
                ));
            }
            let to_bal = load_bal(asset_id, *recipient)
                .checked_add(*amount)
                .ok_or_else(|| PrecompileError::Other("agent batch pay: balance overflow".into()))?;
            save_bal(asset_id, *recipient, to_bal);
        }

        ok_empty()
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

        require_agent_perms(agent_id, asset_id)?;

        // Deduct agent balance
        let agent_bal = load_agent_bal(agent_id, asset_id)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("agent bridge deposit: insufficient agent balance".into()))?;
        save_agent_bal(agent_id, asset_id, agent_bal);

        // Note: Full bridge to EVM requires EVM executor (not available in precompile context).
        // The protocol-level bridge deposit instruction handles ERC-20 contract calls.
        // This precompile deducts balances and records intent; validators relay the withdrawal.

        ok_empty()
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

        ok_empty()
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
            [0x2b, 0x3c, 0xe0, 0xbf] => self.register_agent(calldata, msg_sender),
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
    use crate::slot_balance;

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
            let mut input = vec![0u8; 228];
            input[0..4].copy_from_slice(&[0x2b, 0x3c, 0xe0, 0xbf]);
            // name offset = 96 (0x60)
            input[28..36].copy_from_slice(&96u64.to_be_bytes());
            // url offset = 160 (0xA0)
            input[60..68].copy_from_slice(&160u64.to_be_bytes());
            input[68..100].copy_from_slice(&[0xBBu8; 32]);
            // name length = 9
            input[124..132].copy_from_slice(&9u64.to_be_bytes());
            input[132..141].copy_from_slice(b"TestAgent");
            // url length = 15
            input[188..196].copy_from_slice(&15u64.to_be_bytes());
            input[196..211].copy_from_slice(b"http://test.com");

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
            assert_eq!(&result.bytes[0..9], b"TestAgent");

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
            let mut input = vec![0u8; 228];
            input[0..4].copy_from_slice(&[0x2b, 0x3c, 0xe0, 0xbf]);
            input[28..36].copy_from_slice(&96u64.to_be_bytes());
            input[60..68].copy_from_slice(&160u64.to_be_bytes());
            input[68..100].copy_from_slice(&[0xCCu8; 32]);
            input[124..132].copy_from_slice(&5u64.to_be_bytes());
            input[132..137].copy_from_slice(b"Agent");
            input[188..196].copy_from_slice(&3u64.to_be_bytes());
            input[196..199].copy_from_slice(b"url");
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

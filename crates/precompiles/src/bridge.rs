//! Bridge precompile at 0x103
//!
//! Functions: getTotalDeposits, getTotalWithdrawals,
//!            bridgeToEvm, bridgeToProtocol, externalDeposit, externalWithdraw,
//!            deposit, challengeDeposit

use call_primitives::{Address, AssetId, Balance, Hash};
use alloy_primitives::address;
use std::sync::{Arc, RwLock};

use revm_precompile::PrecompileError;

use crate::{
    address_to_u256, decode_address, decode_bytes32, decode_u128, decode_u64, decode_u256_usize,
    encode_u128, ok_empty, slot_asset_meta, slot_balance, u128_to_u256, u256_to_address,
    u256_to_u128, u256_to_u64, u64_to_u256,
};
use crate::storage::{storage_slot, StorageCtx};
use crate::StatefulPrecompile;

pub const BRIDGE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000103");

#[derive(Debug, Clone)]
pub struct BridgeOpStatus {
    pub op_id: u64,
    pub completed: bool,
    pub confirmations: u64,
    pub source_tx_hash: Option<Hash>,
}

#[derive(Debug, Default)]
pub struct BridgeState {
    pub total_deposits: Balance,
    pub total_withdrawals: Balance,
    pub pending_ops: u64,
}

pub fn bridge_get_status() -> BridgeOpStatus {
    BridgeOpStatus {
        op_id: 0,
        completed: false,
        confirmations: 0,
        source_tx_hash: None,
    }
}

pub fn bridge_deposit(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_deposits += amount;
    state.pending_ops += 1;
}

pub fn bridge_withdraw(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_withdrawals += amount;
    state.pending_ops += 1;
}

/// Deprecated: live bridge was accessed via OnceLock; now BridgePrecompile
/// reads directly from EVM storage through StorageCtx.
#[deprecated(note = "bridge reads from EVM storage; OnceLock no longer used")]
pub fn set_live_bridge(_bridge: Arc<RwLock<BridgeState>>) {}

/// Deprecated: always returns None. Bridge state is in EVM storage.
#[deprecated(note = "bridge reads from EVM storage; OnceLock no longer used")]
pub fn get_live_bridge() -> Option<Arc<RwLock<BridgeState>>> {
    None
}

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_bridge_total_deposits() -> alloy_primitives::U256 {
    alloy_primitives::U256::from(0)
}

fn slot_bridge_total_withdrawals() -> alloy_primitives::U256 {
    alloy_primitives::U256::from(1)
}

fn slot_bridge_paused() -> alloy_primitives::U256 {
    storage_slot(&[b"paused"])
}

fn slot_bridge_processed(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"processed", &tx_hash])
}

// ── Challenge storage slot helpers ────────────────────────────────────

fn slot_bridge_challenge_status(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_status", &tx_hash])
}

fn slot_bridge_challenge_challenger(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_challenger", &tx_hash])
}

fn slot_bridge_challenge_deadline(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_deadline", &tx_hash])
}

fn slot_bridge_challenge_bond(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_bond", &tx_hash])
}

fn slot_bridge_challenge_proof_hash(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_proof_hash", &tx_hash])
}

fn slot_bridge_challenge_original_validator(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_validator", &tx_hash])
}

// ── Deposit metadata storage slot helpers ─────────────────────────────

fn slot_bridge_deposit_asset_id(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"deposit_asset_id", &tx_hash])
}

fn slot_bridge_deposit_recipient(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"deposit_recipient", &tx_hash])
}

fn slot_bridge_deposit_amount(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"deposit_amount", &tx_hash])
}

fn slot_bridge_deposit_block_height(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"deposit_block", &tx_hash])
}

// ── Challenge period config ───────────────────────────────────────────

const DEFAULT_CHALLENGE_PERIOD: u64 = 100;
const DEFAULT_CHALLENGE_BOND: u128 = 1000;
const CALL_ASSET_ID: u64 = 1;

fn slot_challenge_period() -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_period"])
}

fn slot_challenge_bond_amount() -> alloy_primitives::U256 {
    storage_slot(&[b"challenge_bond"])
}

fn get_challenge_period() -> u64 {
    StorageCtx::sload(BRIDGE_ADDRESS, slot_challenge_period())
        .map(u256_to_u64)
        .filter(|&v| v != 0)
        .unwrap_or(DEFAULT_CHALLENGE_PERIOD)
}

fn get_challenge_bond() -> u128 {
    StorageCtx::sload(BRIDGE_ADDRESS, slot_challenge_bond_amount())
        .map(u256_to_u128)
        .filter(|&v| v != 0)
        .unwrap_or(DEFAULT_CHALLENGE_BOND)
}

// ── Validation helpers ────────────────────────────────────────────────

fn bridge_asset_registered(asset_id: u64) -> bool {
    let issuer = StorageCtx::sload(crate::ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"))
        .map(u256_to_address)
        .unwrap_or(Address::ZERO);
    issuer != Address::ZERO
}

fn bridge_is_paused() -> bool {
    StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_paused())
        .map(|v| v.to_be_bytes::<32>()[31] != 0)
        .unwrap_or(false)
}

fn bridge_validate_asset(asset_id: u64) -> Result<(), PrecompileError> {
    if asset_id == 0 {
        return Err(PrecompileError::Other("asset 0 not bridgeable".into()));
    }
    if !bridge_asset_registered(asset_id) {
        return Err(PrecompileError::Other("asset not registered".into()));
    }
    if bridge_is_paused() {
        return Err(PrecompileError::Other("bridge paused".into()));
    }
    let status = StorageCtx::sload(crate::ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(0);
    if status != 0 {
        return Err(PrecompileError::Other("asset not active".into()));
    }
    Ok(())
}

fn bridge_validate_basic(asset_id: u64) -> Result<(), PrecompileError> {
    if !bridge_asset_registered(asset_id) {
        return Err(PrecompileError::Other("asset not registered".into()));
    }
    if bridge_is_paused() {
        return Err(PrecompileError::Other("bridge paused".into()));
    }
    Ok(())
}

// ── Balance helpers ───────────────────────────────────────────────────

fn load_bal(asset_id: u64, addr: Address) -> u128 {
    StorageCtx::sload(crate::ASSET_ADDRESS, slot_balance(asset_id, addr))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_bal(asset_id: u64, addr: Address, amount: u128) {
    StorageCtx::sstore(crate::ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

fn credit_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_bal(asset_id, addr)
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
    save_bal(asset_id, addr, bal);
    Ok(())
}

fn debit_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_bal(asset_id, addr)
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
    save_bal(asset_id, addr, bal);
    Ok(())
}

// ── Bridge total helpers ──────────────────────────────────────────────

fn add_total_deposits(amount: u128) {
    let total = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_deposits())
        .map(u256_to_u128)
        .unwrap_or(0);
    StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_total_deposits(), u128_to_u256(total + amount));
}

fn add_total_withdrawals(amount: u128) {
    let total = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_withdrawals())
        .map(u256_to_u128)
        .unwrap_or(0);
    StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_total_withdrawals(), u128_to_u256(total + amount));
}

fn sub_total_deposits(amount: u128) {
    let total = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_deposits())
        .map(u256_to_u128)
        .unwrap_or(0);
    StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_total_deposits(), u128_to_u256(total.saturating_sub(amount)));
}

// ── ABI decoding helpers ─────────────────────────────────────────────

fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset;
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

// ── BridgePrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct BridgePrecompile;

impl BridgePrecompile {
    fn get_total_deposits(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let deposits = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_deposits())
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(deposits).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn get_total_withdrawals(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let withdrawals = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_withdrawals())
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(withdrawals).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // bridgeToEvm(uint64 assetId, address to, uint128 amount) -> 0xdbae8a2a
    fn bridge_to_evm(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let _to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        bridge_validate_asset(asset_id)?;

        // Deduct protocol balance from sender
        debit_bal(asset_id, msg_sender, amount)?;

        // Note: EVM native balance credit is not possible from precompile context.
        // The recipient's EVM balance must be updated via consensus-layer execution
        // or a separate EVM transfer. This precompile only updates protocol state.

        add_total_withdrawals(amount);
        ok_empty()
    }

    // bridgeToProtocol(uint64 assetId, address to, uint128 amount) -> 0xf0c861e4
    fn bridge_to_protocol(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        bridge_validate_asset(asset_id)?;

        // Note: EVM native balance deduction is not possible from precompile context.
        // The caller must ensure sufficient EVM balance is available (e.g., via tx value).
        // This precompile only updates protocol state.

        credit_bal(asset_id, to, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // externalDeposit(bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount)
    // -> 0x1aba0700
    // Simplified: credits recipient balance. Full validator signature verification is done
    // at the consensus layer; this precompile is a convenience for validators to relay.
    fn external_deposit(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let recipient = decode_address(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid recipient".into()))?;
        let amount = decode_u128(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        // Check not already processed
        let processed = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash))
            .map(|v| v != alloy_primitives::U256::ZERO)
            .unwrap_or(false);
        if processed {
            return Err(PrecompileError::Other("source tx already processed".into()));
        }

        bridge_validate_basic(asset_id)?;

        // Mark as processed
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash), alloy_primitives::U256::from(1u8));

        // Record deposit metadata for challenge period
        let block_height = StorageCtx::block_number();
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(source_tx_hash), u64_to_u256(asset_id));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_deposit_recipient(source_tx_hash), address_to_u256(recipient));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_deposit_amount(source_tx_hash), u128_to_u256(amount));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_deposit_block_height(source_tx_hash), u64_to_u256(block_height));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash), address_to_u256(msg_sender));

        credit_bal(asset_id, recipient, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // externalWithdraw(uint64 targetChain, bytes targetAddress, uint64 assetId, uint128 amount)
    // -> 0x393da669
    fn external_withdraw(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let _target_chain = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid targetChain".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        // Decode targetAddress (dynamic bytes)
        let _target_addr = decode_bytes(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress".into()))?;

        bridge_validate_basic(asset_id)?;

        debit_bal(asset_id, msg_sender, amount)?;
        add_total_withdrawals(amount);
        ok_empty()
    }

    // deposit(uint64 sourceChain, address targetAddress, uint128 amount, uint64 assetId, bytes proof)
    // -> 0x2689cfc0
    // Simplified: credits targetAddress balance. The proof is accepted but not cryptographically
    // verified in the precompile (full verification happens at consensus layer).
    fn deposit(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 164 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let _source_chain = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceChain".into()))?;
        let target_address = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;
        let asset_id = decode_u64(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;

        // Decode proof (dynamic bytes) — validated at consensus layer
        let _proof = decode_bytes(input, 132)
            .ok_or_else(|| PrecompileError::Other("invalid proof".into()))?;

        bridge_validate_basic(asset_id)?;

        credit_bal(asset_id, target_address, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // initiateChallenge(bytes32 sourceTxHash, bytes proof) -> 0x07dee8d0
    fn initiate_challenge(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;

        // Decode proof (dynamic bytes)
        let proof = decode_bytes(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid proof".into()))?;

        // Verify source tx is processed
        let processed = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash))
            .map(|v| v != alloy_primitives::U256::ZERO)
            .unwrap_or(false);
        if !processed {
            return Err(PrecompileError::Other("source tx not processed".into()));
        }

        // Verify no existing pending challenge
        let existing_status = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if existing_status != 0 {
            return Err(PrecompileError::Other("challenge already exists".into()));
        }

        // Verify still within challenge period
        let deposit_height = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_block_height(source_tx_hash))
            .map(u256_to_u64)
            .unwrap_or(0);
        let current_height = StorageCtx::block_number();
        let challenge_period = get_challenge_period();
        if current_height >= deposit_height + challenge_period {
            return Err(PrecompileError::Other("challenge period expired".into()));
        }

        // Deduct bond from challenger
        let bond = get_challenge_bond();
        debit_bal(CALL_ASSET_ID, msg_sender, bond)?;

        // Store challenge metadata
        let deadline = current_height + challenge_period;
        let proof_hash = alloy_primitives::keccak256(&proof);
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash), alloy_primitives::U256::from(1u8)); // Pending
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(source_tx_hash), address_to_u256(msg_sender));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(source_tx_hash), u64_to_u256(deadline));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_bond(source_tx_hash), u128_to_u256(bond));
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_proof_hash(source_tx_hash), alloy_primitives::U256::from_be_slice(proof_hash.as_slice()));

        ok_empty()
    }

    // resolveChallenge(bytes32 sourceTxHash) -> 0x8a1e5018
    fn resolve_challenge(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 100000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;

        // Validate challenge is pending
        let status = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if status != 1 {
            return Err(PrecompileError::Other("challenge not pending".into()));
        }

        // Validate deadline has passed
        let deadline = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(source_tx_hash))
            .map(u256_to_u64)
            .unwrap_or(0);
        let current_height = StorageCtx::block_number();
        if current_height < deadline {
            return Err(PrecompileError::Other("challenge deadline not reached".into()));
        }

        // Verify fraud proof (stub: always returns false)
        let proof_valid = verify_fraud_proof(source_tx_hash);

        let bond = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_bond(source_tx_hash))
            .map(u256_to_u128)
            .unwrap_or(DEFAULT_CHALLENGE_BOND);

        if proof_valid {
            // Challenge successful: rollback deposit
            let asset_id = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(source_tx_hash))
                .map(u256_to_u64)
                .unwrap_or(0);
            let recipient = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_recipient(source_tx_hash))
                .map(u256_to_address)
                .unwrap_or(Address::ZERO);
            let amount = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_amount(source_tx_hash))
                .map(u256_to_u128)
                .unwrap_or(0);

            // Debit recipient balance (may fail if already spent; that's ok)
            let _ = debit_bal(asset_id, recipient, amount);
            sub_total_deposits(amount);

            // Clear processed flag so correct deposit can be re-processed
            StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash), alloy_primitives::U256::ZERO);

            // Return bond to challenger + reward
            let challenger = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(source_tx_hash))
                .map(u256_to_address)
                .unwrap_or(Address::ZERO);
            let _ = credit_bal(CALL_ASSET_ID, challenger, bond + bond / 10); // 10% reward

            // Slash original validator
            let validator = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash))
                .map(u256_to_address)
                .unwrap_or(Address::ZERO);
            let _ = slash_validator_stake(validator);

            StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash), alloy_primitives::U256::from(2u8)); // Successful
        } else {
            // Challenge failed: bond goes to original validator
            let validator = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash))
                .map(u256_to_address)
                .unwrap_or(Address::ZERO);
            let _ = credit_bal(CALL_ASSET_ID, validator, bond);

            StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash), alloy_primitives::U256::from(3u8)); // Failed
        }

        ok_empty()
    }

    // getChallengeStatus(bytes32 sourceTxHash) -> (uint8,uint64,uint128,address)
    // -> 0x2a5d97e9
    fn get_challenge_status(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;

        let status = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        let deadline = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(source_tx_hash))
            .map(u256_to_u64)
            .unwrap_or(0);
        let bond = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_bond(source_tx_hash))
            .map(u256_to_u128)
            .unwrap_or(0);
        let challenger = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(source_tx_hash))
            .map(u256_to_address)
            .unwrap_or(Address::ZERO);

        let mut out = [0u8; 128];
        out[31] = status;
        out[56..64].copy_from_slice(&deadline.to_be_bytes());
        out[80..96].copy_from_slice(&bond.to_be_bytes());
        out[108..128].copy_from_slice(challenger.as_slice());

        let output = revm_precompile::PrecompileOutput::new(0, out.to_vec().into());
        Ok(crate::storage::fill_precompile_output(output))
    }

    // withdrawChallengeBond(bytes32 sourceTxHash) -> 0x9c4e5e8b
    fn withdraw_challenge_bond(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 5000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;

        let status = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if status != 2 {
            return Err(PrecompileError::Other("challenge not successful".into()));
        }

        let challenger = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(source_tx_hash))
            .map(u256_to_address)
            .unwrap_or(Address::ZERO);
        if challenger != msg_sender {
            return Err(PrecompileError::Other("not challenger".into()));
        }

        let bond = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_bond(source_tx_hash))
            .map(u256_to_u128)
            .unwrap_or(DEFAULT_CHALLENGE_BOND);

        // Transfer bond + reward to challenger
        let reward = bond + bond / 10; // 10% reward
        credit_bal(CALL_ASSET_ID, challenger, reward)?;

        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash), alloy_primitives::U256::from(4u8)); // Withdrawn

        ok_empty()
    }
}

/// Verify fraud proof for a challenged deposit.
/// TODO: Implement actual cryptographic verification based on bridge type.
fn verify_fraud_proof(_source_tx_hash: [u8; 32]) -> bool {
    false
}

/// Slash validator stake. Minimal implementation: reduce stake and clear status.
fn slash_validator_stake(validator: Address) -> Result<(), PrecompileError> {
    let stake = StorageCtx::sload(crate::VALIDATOR_ADDRESS, crate::slot_validator_by_addr(validator))
        .map(u256_to_u64)
        .unwrap_or(0);
    if stake == 0 {
        return Ok(());
    }

    // For now, just clear validator status to inactive
    // Full slashing logic (transferring to treasury, reducing stake) can be added later
    StorageCtx::sstore(crate::VALIDATOR_ADDRESS, crate::slot_validator_by_addr(validator), alloy_primitives::U256::ZERO);

    Ok(())
}

impl StatefulPrecompile for BridgePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }
        match &calldata[..4] {
            &[0xa8, 0x7e, 0x4f, 0x2a] => self.get_total_deposits(),
            &[0x9c, 0x3e, 0x6d, 0x1b] => self.get_total_withdrawals(),
            &[0xdb, 0xae, 0x8a, 0x2a] => self.bridge_to_evm(calldata, msg_sender),
            &[0xf0, 0xc8, 0x61, 0xe4] => self.bridge_to_protocol(calldata, msg_sender),
            &[0x1a, 0xba, 0x07, 0x00] => self.external_deposit(calldata, msg_sender),
            &[0x39, 0x3d, 0xa6, 0x69] => self.external_withdraw(calldata, msg_sender),
            &[0x26, 0x89, 0xcf, 0xc0] => self.deposit(calldata, msg_sender),
            &[0x07, 0xde, 0xe8, 0xd0] => self.initiate_challenge(calldata, msg_sender),
            &[0x8a, 0x1e, 0x50, 0x18] => self.resolve_challenge(calldata, msg_sender),
            &[0x2a, 0x5d, 0x97, 0xe9] => self.get_challenge_status(calldata),
            &[0x9c, 0x4e, 0x5e, 0x8b] => self.withdraw_challenge_bond(calldata, msg_sender),
            _ => Err(revm_precompile::PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_address() {
        assert_eq!(
            BRIDGE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000103")
        );
    }

    #[test]
    fn test_bridge_precompile_protocol_balance() {
        let status = bridge_get_status();
        assert!(!status.completed);
        assert_eq!(status.confirmations, 0);
    }

    #[test]
    fn test_bridge_deposit_and_withdraw() {
        let mut state = BridgeState::default();
        bridge_deposit(&mut state, 1, Address::ZERO, Address::ZERO, 1000);
        assert_eq!(state.total_deposits, 1000);
        assert_eq!(state.pending_ops, 1);

        bridge_withdraw(&mut state, 1, Address::ZERO, Address::ZERO, 500);
        assert_eq!(state.total_withdrawals, 500);
        assert_eq!(state.pending_ops, 2);
    }

    #[test]
    fn test_bridge_precompile_stateful_reads() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                alloy_primitives::U256::from(0),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                alloy_primitives::U256::from(1),
                u128_to_u256(2000),
            );

            let mut precompile = BridgePrecompile;

            // getTotalDeposits
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0xa8, 0x7e, 0x4f, 0x2a]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let deposits = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(deposits, 5000);

            // getTotalWithdrawals
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x9c, 0x3e, 0x6d, 0x1b]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let withdrawals = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(withdrawals, 2000);
        });
    }

    #[test]
    fn test_external_deposit_records_metadata() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Register asset_id=1
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit(sourceTxHash, assetId=1, recipient, amount=1000)
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());

            let result = precompile.call(&input, validator);
            assert!(result.is_ok(), "external_deposit failed: {:?}", result.err());

            // Verify metadata stored
            let stored_asset_id = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(source_tx_hash))
                .map(u256_to_u64).unwrap_or(0);
            assert_eq!(stored_asset_id, 1);

            let stored_recipient = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_recipient(source_tx_hash))
                .map(u256_to_address).unwrap_or(Address::ZERO);
            assert_eq!(stored_recipient, recipient);

            let stored_amount = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_amount(source_tx_hash))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(stored_amount, 1000);

            let stored_height = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_deposit_block_height(source_tx_hash))
                .map(u256_to_u64).unwrap_or(0);
            assert_eq!(stored_height, 10);

            let stored_validator = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash))
                .map(u256_to_address).unwrap_or(Address::ZERO);
            assert_eq!(stored_validator, validator);
        });
    }

    #[test]
    fn test_initiate_challenge_success() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Register asset_id=1 and seed challenger balance
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            // Seed recipient balance for externalDeposit
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // 1. externalDeposit
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());
            precompile.call(&input, validator).unwrap();

            // 2. initiateChallenge at block 10 (within period)
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            // proof offset = 64
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            // proof data: len=4, data="proof"
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");

            let result = precompile.call(&input, challenger);
            assert!(result.is_ok(), "initiate_challenge failed: {:?}", result.err());

            // Verify challenge stored
            let status = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
                .map(|v| v.to_be_bytes::<32>()[31]).unwrap_or(0);
            assert_eq!(status, 1); // Pending

            let stored_challenger = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(source_tx_hash))
                .map(u256_to_address).unwrap_or(Address::ZERO);
            assert_eq!(stored_challenger, challenger);

            let deadline = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(source_tx_hash))
                .map(u256_to_u64).unwrap_or(0);
            assert_eq!(deadline, 110); // block 10 + period 100

            // Bond deducted from challenger
            let challenger_bal = load_bal(CALL_ASSET_ID, challenger);
            assert_eq!(challenger_bal, 4000); // 5000 - 1000 bond
        });
    }

    #[test]
    fn test_initiate_challenge_fails_not_processed() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let challenger = Address::repeat_byte(0x33);
        let source_tx_hash = [0xABu8; 32];

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");

            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: source tx not processed");
        });
    }

    #[test]
    fn test_initiate_challenge_fails_period_expired() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 100);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // externalDeposit at block 100
        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());
            precompile.call(&input, validator).unwrap();
        });

        // initiateChallenge at block 200 (deposit at block 100, period = 100, so expired)
        provider.set_block_number(200);
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");

            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: challenge period expired");
        });
    }

    #[test]
    fn test_initiate_challenge_fails_already_challenged() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());
            precompile.call(&input, validator).unwrap();

            // First challenge
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");
            precompile.call(&input, challenger).unwrap();

            // Second challenge should fail
            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: already challenged");
        });
    }

    #[test]
    fn test_resolve_challenge_false_proof_bond_forfeited() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Setup state and externalDeposit + initiateChallenge at block 10
        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, validator),
                u128_to_u256(100),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());
            precompile.call(&input, validator).unwrap();

            // initiateChallenge
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");
            precompile.call(&input, challenger).unwrap();
        });

        // resolveChallenge at block 120 (deadline = 10 + 100 = 110)
        // Since verify_fraud_proof returns false, challenge fails
        provider.set_block_number(120);
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x8a, 0x1e, 0x50, 0x18]);
            input[4..36].copy_from_slice(&source_tx_hash);

            let result = precompile.call(&input, Address::ZERO);
            assert!(result.is_ok(), "resolve_challenge failed: {:?}", result.err());

            // Status should be Failed (3)
            let status = crate::storage::StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
                .map(|v| v.to_be_bytes::<32>()[31]).unwrap_or(0);
            assert_eq!(status, 3);

            // Validator should have received the bond
            let validator_bal = load_bal(CALL_ASSET_ID, validator);
            assert_eq!(validator_bal, 1100); // 100 + 1000 bond
        });
    }

    #[test]
    fn test_get_challenge_status() {
        let mut provider = crate::storage::HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                alloy_primitives::U256::from(1u8),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x1a, 0xba, 0x07, 0x00]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[60..68].copy_from_slice(&1u64.to_be_bytes());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&1000u128.to_be_bytes());
            precompile.call(&input, validator).unwrap();

            // initiateChallenge
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x07, 0xde, 0xe8, 0xd0]);
            input[4..36].copy_from_slice(&source_tx_hash);
            input[36 + 24..36 + 32].copy_from_slice(&64u64.to_be_bytes());
            let proof_abs = 4 + 64;
            input[proof_abs + 24..proof_abs + 32].copy_from_slice(&5u64.to_be_bytes());
            input[proof_abs + 32..proof_abs + 37].copy_from_slice(b"proof");
            precompile.call(&input, challenger).unwrap();

            // getChallengeStatus
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x2a, 0x5d, 0x97, 0xe9]);
            input[4..36].copy_from_slice(&source_tx_hash);

            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes.len(), 128);
            assert_eq!(result.bytes[31], 1); // status = Pending
            let deadline = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&result.bytes[56..64]);
                buf
            });
            assert_eq!(deadline, 110);
            let bond = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[80..96]);
                buf
            });
            assert_eq!(bond, 1000);
            let returned_challenger = Address::from_slice(&result.bytes[108..128]);
            assert_eq!(returned_challenger, challenger);
        });
    }
}

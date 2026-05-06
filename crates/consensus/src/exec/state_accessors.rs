//! EVM-based protocol instruction execution.
//!
//! Asset instructions (Transfer, BatchTransfer, Approve, TransferFrom,
//! Mint, Burn) are executed by reading/writing EVM storage directly,
//! using the same slot layout as the AssetPrecompile (0x201).
//!
//! This replaces the legacy `AccountState` / `AssetRegistry` mutation path
//! so that the block state_root captures all state changes.

use call_asset::AssetStorage;
use call_evm::{ProtocolStorage, ProtocolStateBackend, ProtocolStateRefBackend};
use call_precompile::{
    address_to_u256, read_string32,
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256,
    write_string32,
    AGENT_ADDRESS, BRIDGE_ADDRESS, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
    ORACLE_ADDRESS, SHIELDED_ADDRESS, VALIDATOR_ADDRESS,
};
use call_precompile::storage::storage_slot;
use call_primitives::{Address, U256};
// ── Constants ─────────────────────────────────────────────────────────

#[allow(dead_code)]
/// Default unbonding period in blocks (~8.4h at 250ms block time)
const UNBONDING_PERIOD_BLOCKS: u64 = 120_960;
#[allow(dead_code)]
/// Minimum self-stake to become a validator
const MIN_SELF_STAKE: u128 = 1_000_000;
#[allow(dead_code)]
/// Proposal deposit in CALL
const PROPOSAL_DEPOSIT: u128 = 10_000;
#[allow(dead_code)]
/// Governance timelock in blocks
const GOV_TIMELOCK_BLOCKS: u64 = 100;
#[allow(dead_code)]
/// Governance quorum threshold (basis points)
const GOV_QUORUM_BPS: u128 = 3_333;

// ── Storage slot helpers (shared with precompiles) ────────────────────

fn slot_validator_count() -> U256 {
    U256::ZERO
}

fn slot_validator_by_addr(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"validator_id"])
}

fn slot_validator_addr(index: u64) -> U256 {
    storage_slot(&[b"validators"]) + U256::from(index)
}

fn slot_validator_stake(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"stake"])
}

fn slot_validator_pubkey(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"pubkey"])
}

fn slot_validator_status(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"status"])
}

#[allow(dead_code)]
fn _slot_validator_unbond_height(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"unbond_at"])
}

fn slot_validator_bls_pubkey(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"bls_pubkey"])
}

#[allow(dead_code)]
fn _slot_unbonding_count() -> U256 {
    U256::from(1)
}

#[allow(dead_code)]
fn _slot_unbonding(index: u64) -> U256 {
    storage_slot(&[b"unbonding"]) + U256::from(index)
}

fn slot_oracle(asset_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], suffix])
}

fn slot_compliance(addr: Address, policy_id: u8) -> U256 {
    storage_slot(&[addr.as_slice(), &[policy_id]])
}

fn slot_gov_proposal_count() -> U256 {
    U256::ZERO
}

pub fn slot_gov_proposal(proposal_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], suffix])
}

pub fn slot_gov_proposal_data_len(proposal_id: u64) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], b"data_len"])
}

pub fn slot_gov_proposal_data_chunk(proposal_id: u64, chunk_idx: u64) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], b"data_chunk", &chunk_idx.to_be_bytes()[..]])
}

fn slot_gov_voter(proposal_id: u64, voter: Address) -> U256 {
    storage_slot(&[b"vote", &proposal_id.to_be_bytes()[..], voter.as_slice()])
}

pub fn slot_gov_paused() -> U256 {
    storage_slot(&[b"paused"])
}

#[allow(dead_code)]
fn _slot_gov_pause_reason() -> U256 {
    storage_slot(&[b"pause_reason"])
}

#[allow(dead_code)]
fn _slot_gov_proposal_deposit() -> U256 {
    storage_slot(&[b"deposit_fee"])
}

// ── Bridge storage slot helpers ───────────────────────────────────────

fn slot_bridge_total_deposits(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"deposits"])
}

fn slot_bridge_total_withdrawals(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"withdrawals"])
}

fn slot_bridge_paused(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"paused"])
}

fn slot_bridge_daily_used(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"daily"])
}

fn slot_bridge_daily_day(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"daily_day"])
}

fn slot_bridge_external_paused() -> U256 {
    storage_slot(&[b"external_paused"])
}

fn slot_bridge_processed(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"processed", &source_tx_hash])
}

pub(crate) fn slot_bridge_contract(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"contract"])
}

fn slot_bridge_pending_count() -> U256 {
    storage_slot(&[b"pending_count"])
}

fn slot_bridge_pending_hash(index: u64) -> U256 {
    storage_slot(&[b"pending_list"]) + U256::from(index)
}

fn slot_bridge_pending_status(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_status", &source_tx_hash])
}

fn slot_bridge_pending_recipient(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_recipient", &source_tx_hash])
}

fn slot_bridge_pending_asset(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_asset", &source_tx_hash])
}

fn slot_bridge_pending_amount(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_amount", &source_tx_hash])
}

fn slot_bridge_pending_block(source_tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"pending_block", &source_tx_hash])
}

#[allow(dead_code)]
fn _slot_bridge_withdrawal_period_start() -> U256 {
    storage_slot(&[b"withdrawal_start"])
}

#[allow(dead_code)]
fn _slot_bridge_withdrawal_period_used(asset_id: u64) -> U256 {
    storage_slot(&[b"withdrawal_period", &asset_id.to_be_bytes()[..]])
}

// ── Agent storage slot helpers ────────────────────────────────────────

pub(crate) fn slot_agent_count() -> U256 {
    U256::ZERO
}

pub(crate) fn slot_agent_owner(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"owner"])
}

pub(crate) fn slot_agent_pubkey(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"pubkey"])
}

pub(crate) fn slot_agent_name(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"name"])
}

pub(crate) fn slot_agent_url(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"url"])
}

pub(crate) fn slot_agent_perms(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"perms"])
}

pub(crate) fn slot_agent_registered_at(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"block"])
}

pub(crate) fn slot_agent_balance(agent_id: u64, asset_id: u64) -> U256 {
    storage_slot(&[b"abalance", &agent_id.to_be_bytes()[..], &asset_id.to_be_bytes()[..]])
}

// Pack agent permissions into a single U256:
// bytes 0..16  = per_tx_limit (u128)
// bytes 16..24 = expires_at (u64)
// byte 31      = flags (bit 0 = allow asset 1, bit 1 = allow all protocols)
pub(crate) fn pack_agent_perms(per_tx_limit: u128, expires_at: u64, flags: u8) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..24].copy_from_slice(&expires_at.to_be_bytes());
    packed[31] = flags;
    U256::from_be_slice(&packed)
}

#[allow(dead_code)]
pub(crate) fn _unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit = u128::from_be_bytes(bytes[0..16].try_into().unwrap());
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

pub fn agent_exists(evm_state: &dyn ProtocolStorage, agent_id: u64) -> bool {
    evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)) != U256::ZERO
}

pub fn agent_get_owner(evm_state: &dyn ProtocolStorage, agent_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)))
}


pub fn agent_get_balance(evm_state: &dyn ProtocolStorage, agent_id: u64, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id)))
}

pub fn agent_set_balance(evm_state: &mut dyn ProtocolStorage, agent_id: u64, asset_id: u64, amount: u128) {
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_balance(agent_id, asset_id),
        u128_to_u256(amount),
    );
}

/// Set agent pubkey hash in EVM storage.
pub fn agent_set_pubkey(evm_state: &mut dyn ProtocolStorage, agent_id: u64, pubkey: &[u8; 32]) {
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_pubkey(agent_id),
        U256::from_be_slice(pubkey),
    );
}

// ── Helpers for tests / genesis / RPC ─────────────────────────────────

/// Seed an asset balance directly into EVM storage.
pub fn seed_balance(evm_state: &mut dyn ProtocolStorage, asset_id: u64, addr: Address, amount: u128) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.write_balance(asset_id, addr, amount);
}

/// Add to an asset balance in EVM storage (reads current, adds amount, writes back).
pub fn add_balance_evm(evm_state: &mut dyn ProtocolStorage, asset_id: u64, addr: Address, amount: u128) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.add_balance(asset_id, addr, amount).ok();
}

/// Deduct from an asset balance in EVM storage (reads current, subtracts amount, writes back).
/// Returns true if deduction succeeded, false if insufficient balance.
pub fn deduct_balance_evm(evm_state: &mut dyn ProtocolStorage, asset_id: u64, addr: Address, amount: u128) -> bool {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.deduct_balance(asset_id, addr, amount).is_ok()
}

/// Seed an allowance directly into EVM storage.
pub fn seed_allowance(evm_state: &mut dyn ProtocolStorage, asset_id: u64, owner: Address, spender: Address, amount: u128) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.write_allowance(asset_id, owner, spender, amount);
}

/// Read an allowance from EVM storage.
pub fn read_allowance(evm_state: &dyn ProtocolStorage, asset_id: u64, owner: Address, spender: Address) -> u128 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_allowance(asset_id, owner, spender)
}

/// Add to asset supply in EVM storage.
pub fn add_asset_supply_evm(evm_state: &mut dyn ProtocolStorage, asset_id: u64, amount: u128) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    let current = store.read_meta(asset_id).supply;
    let new = current.saturating_add(amount);
    store.store_meta_u256(asset_id, b"supply", u128_to_u256(new));
}

/// Seed asset metadata directly into EVM storage.
#[allow(clippy::too_many_arguments)]
pub fn seed_asset(
    evm_state: &mut dyn ProtocolStorage,
    asset_id: u64,
    symbol: &str,
    name: &str,
    decimals: u8,
    issuer: Address,
    max_supply: u128,
    supply: u128,
    status: u8,
) {
    use call_asset::AssetMeta;
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.write_meta(
        asset_id,
        &AssetMeta {
            symbol: symbol.to_string(),
            name: name.to_string(),
            decimals,
            issuer,
            max_supply,
            supply,
            status,
        },
    );
    // Compliance and registered_at defaults
    store.store_meta_u256(asset_id, b"compliance", call_primitives::U256::from(0));
    store.store_meta_u256(asset_id, b"registered_at", call_primitives::U256::from(0));
}

/// Read an asset balance from EVM storage.
pub fn read_balance(evm_state: &dyn ProtocolStorage, asset_id: u64, addr: Address) -> u128 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_balance(asset_id, addr)
}

/// Read asset info from EVM storage.
pub fn read_asset_symbol(evm_state: &dyn ProtocolStorage, asset_id: u64) -> String {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).symbol
}

/// Read asset issuer from EVM storage.
pub fn read_asset_issuer(evm_state: &dyn ProtocolStorage, asset_id: u64) -> Address {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).issuer
}

/// Read asset status from EVM storage.
pub fn read_asset_status(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u8 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).status
}

/// Seed bridge contract address for an asset in EVM storage.
pub fn seed_bridge_contract(evm_state: &mut dyn ProtocolStorage, asset_id: u64, contract: Address) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(asset_id), address_to_u256(contract));
}

/// Read total bridge deposits for an asset from EVM storage.
pub fn read_bridge_total_deposits(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_deposits(asset_id)))
}

/// Read total bridge withdrawals for an asset from EVM storage.
pub fn read_bridge_total_withdrawals(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_withdrawals(asset_id)))
}

/// Read bridge pending deposit count from EVM storage.
pub fn read_bridge_pending_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_count()))
}

/// Read bridge pending deposit hash by index from EVM storage.
pub fn read_bridge_pending_hash(evm_state: &dyn ProtocolStorage, index: u64) -> [u8; 32] {
    let val = evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_hash(index));
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&val.to_be_bytes::<32>());
    hash
}

/// Read bridge pending deposit status from EVM storage.
pub fn read_bridge_pending_status(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> u8 {
    evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_status(tx_hash)).to_be_bytes::<32>()[31]
}

/// Read bridge pending deposit recipient from EVM storage.
pub fn read_bridge_pending_recipient(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> Address {
    u256_to_address(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_recipient(tx_hash)))
}

/// Read bridge pending deposit asset_id from EVM storage.
pub fn read_bridge_pending_asset(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> u64 {
    u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_asset(tx_hash)))
}

/// Read bridge pending deposit amount from EVM storage.
pub fn read_bridge_pending_amount(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_amount(tx_hash)))
}

/// Read bridge pending deposit block from EVM storage.
pub fn read_bridge_pending_block(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> u64 {
    u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_block(tx_hash)))
}

/// Queue a pending external deposit in EVM storage.
pub fn seed_bridge_pending(
    evm_state: &mut dyn ProtocolStorage,
    tx_hash: [u8; 32],
    recipient: Address,
    asset_id: u64,
    amount: u128,
    block: u64,
) {
    let count = read_bridge_pending_count(evm_state);
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_hash(count), alloy_primitives::U256::from_be_slice(&tx_hash));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_count(), u64_to_u256(count + 1));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_status(tx_hash), alloy_primitives::U256::from(1u8));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_recipient(tx_hash), address_to_u256(recipient));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_asset(tx_hash), u64_to_u256(asset_id));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_amount(tx_hash), u128_to_u256(amount));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_block(tx_hash), u64_to_u256(block));
}

/// Set bridge pending deposit status in EVM storage (0=pending, 2=finalized, 3=rejected).
pub fn set_bridge_pending_status(evm_state: &mut dyn ProtocolStorage, tx_hash: [u8; 32], status: u8) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_status(tx_hash), alloy_primitives::U256::from(status));
}

/// Read whether a source tx has been processed from EVM storage.
pub fn read_bridge_processed(evm_state: &dyn ProtocolStorage, tx_hash: [u8; 32]) -> bool {
    evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_processed(tx_hash)) != U256::ZERO
}

/// Mark a source tx as processed in EVM storage.
pub fn seed_bridge_processed(evm_state: &mut dyn ProtocolStorage, tx_hash: [u8; 32], block_height: u64) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_processed(tx_hash), u64_to_u256(block_height));
}

/// Remove a pending deposit from the EVM pending list by swapping with last.
pub fn remove_bridge_pending(evm_state: &mut dyn ProtocolStorage, tx_hash: [u8; 32]) {
    let count = read_bridge_pending_count(evm_state);
    if count == 0 {
        return;
    }
    // Find the index of the deposit to remove
    let mut index = None;
    for i in 0..count {
        let hash = read_bridge_pending_hash(evm_state, i);
        if hash == tx_hash {
            index = Some(i);
            break;
        }
    }
    let Some(index) = index else { return };
    // Swap with last and decrement count
    if index < count - 1 {
        let last_hash = read_bridge_pending_hash(evm_state, count - 1);
        evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_hash(index), alloy_primitives::U256::from_be_slice(&last_hash));
    }
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_hash(count - 1), U256::ZERO);
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_count(), u64_to_u256(count - 1));
}

/// Read bridge daily usage for an asset from EVM storage.
pub fn read_bridge_daily_used(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id)))
}

/// Read bridge daily usage reset block from EVM storage.
pub fn read_bridge_daily_day(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_daily_day(asset_id)))
}

/// Update bridge daily usage for an asset in EVM storage.
pub fn update_bridge_daily(evm_state: &mut dyn ProtocolStorage, asset_id: u64, used: u128, day: u64) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id), u128_to_u256(used));
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_daily_day(asset_id), u64_to_u256(day));
}

/// Read whether external bridge is globally paused from EVM storage.
pub fn read_bridge_external_paused(evm_state: &dyn ProtocolStorage) -> bool {
    evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_external_paused()) != U256::ZERO
}

/// Set external bridge pause status in EVM storage.
pub fn seed_bridge_external_paused(evm_state: &mut dyn ProtocolStorage, paused: bool) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_external_paused(), if paused { U256::from(1u8) } else { U256::ZERO });
}

/// Read whether bridge is paused for a specific asset from EVM storage.
pub fn read_bridge_paused(evm_state: &dyn ProtocolStorage, asset_id: u64) -> bool {
    evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_paused(asset_id)) != U256::ZERO
}

/// Set bridge pause status for a specific asset in EVM storage.
pub fn seed_bridge_paused(evm_state: &mut dyn ProtocolStorage, asset_id: u64, paused: bool) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_paused(asset_id), if paused { U256::from(1u8) } else { U256::ZERO });
}

/// Finalize pending external deposits whose challenge period has expired.
/// Credits recipient balances and increases asset supply for all finalized deposits.
/// Returns the number of deposits finalized.
pub fn finalize_pending_external_deposits_evm(
    evm_state: &mut dyn ProtocolStorage,
    current_block: u64,
    challenge_period_blocks: u64,
) -> usize {
    let count = read_bridge_pending_count(evm_state);
    if count == 0 {
        return 0;
    }

    let mut finalized = 0;
    // Iterate in reverse so we can safely remove by swapping with last
    let mut i = count;
    while i > 0 {
        i -= 1;
        let tx_hash = read_bridge_pending_hash(evm_state, i);
        if tx_hash == [0u8; 32] {
            continue;
        }
        let status = read_bridge_pending_status(evm_state, tx_hash);
        if status != 1 {
            // Not pending (already finalized or rejected)
            continue;
        }
        let submitted_at = read_bridge_pending_block(evm_state, tx_hash);
        if current_block >= submitted_at + challenge_period_blocks {
            let recipient = read_bridge_pending_recipient(evm_state, tx_hash);
            let asset_id = read_bridge_pending_asset(evm_state, tx_hash);
            let amount = read_bridge_pending_amount(evm_state, tx_hash);

            // Credit recipient balance
            add_balance_evm(evm_state, asset_id, recipient, amount);
            // Increase asset supply
            add_asset_supply_evm(evm_state, asset_id, amount);
            // Mark as finalized
            set_bridge_pending_status(evm_state, tx_hash, 2);

            finalized += 1;
        }
    }

    // Compact the pending list: rebuild only with pending deposits
    let mut new_count = 0u64;
    for i in 0..count {
        let tx_hash = read_bridge_pending_hash(evm_state, i);
        if tx_hash == [0u8; 32] {
            continue;
        }
        let status = read_bridge_pending_status(evm_state, tx_hash);
        if status == 1 {
            // Still pending, keep in list
            if new_count != i {
                evm_state.set_storage(
                    BRIDGE_ADDRESS,
                    slot_bridge_pending_hash(new_count),
                    alloy_primitives::U256::from_be_slice(&tx_hash),
                );
            }
            new_count += 1;
        }
    }
    // Zero out removed entries
    for i in new_count..count {
        evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_hash(i), U256::ZERO);
    }
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_count(), u64_to_u256(new_count));

    finalized
}

// ── Asset read helpers ────────────────────────────────────────────────

/// Read asset name from EVM storage.
pub fn read_asset_name(evm_state: &dyn ProtocolStorage, asset_id: u64) -> String {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).name
}

/// Read asset decimals from EVM storage.
pub fn read_asset_decimals(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u8 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).decimals
}

/// Read asset supply from EVM storage.
pub fn read_asset_supply(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).supply
}

/// Read asset max supply from EVM storage.
pub fn read_asset_max_supply(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.read_meta(asset_id).max_supply
}

/// Read asset compliance policy from EVM storage.
pub fn read_asset_compliance(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u8 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.load_meta_u8(asset_id, b"compliance")
}

/// Set asset compliance policy in EVM storage.
pub fn seed_asset_compliance(evm_state: &mut dyn ProtocolStorage, asset_id: u64, policy: u8) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.store_meta_u256(asset_id, b"compliance", call_primitives::U256::from(policy));
}

/// Read asset contract address from EVM storage.
pub fn read_asset_contract_address(evm_state: &dyn ProtocolStorage, asset_id: u64) -> Option<Address> {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    let val = store.load_meta_u256(asset_id, b"contract");
    if val.is_zero() {
        None
    } else {
        Some(u256_to_address(val))
    }
}

/// Set asset contract address in EVM storage.
pub fn seed_asset_contract_address(evm_state: &mut dyn ProtocolStorage, asset_id: u64, addr: Address) {
    let mut store = AssetStorage::new(ProtocolStateBackend(evm_state));
    store.store_meta_u256(asset_id, b"contract", address_to_u256(addr));
}

/// Read asset registered_at from EVM storage.
pub fn read_asset_registered_at(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u64 {
    let store = AssetStorage::new(ProtocolStateRefBackend(evm_state));
    store.load_meta_u256(asset_id, b"registered_at")
        .try_into()
        .map(|v: u128| v as u64)
        .unwrap_or(0)
}

// ── Agent read helpers ────────────────────────────────────────────────

/// Read agent name from EVM storage.
pub fn agent_get_name(evm_state: &dyn ProtocolStorage, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_name(agent_id)))
}

/// Read agent url from EVM storage.
pub fn agent_get_url(evm_state: &dyn ProtocolStorage, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_url(agent_id)))
}

/// Read agent registered_at from EVM storage.
pub fn agent_get_registered_at(evm_state: &dyn ProtocolStorage, agent_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_registered_at(agent_id)))
}

/// Read total agent count from EVM storage.
pub fn read_agent_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_count()))
}

// ── Validator read helpers ────────────────────────────────────────────

/// Read validator count from EVM storage.
pub fn read_validator_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()))
}

/// Read validator address by validator ID from EVM storage.
pub fn read_validator_addr(evm_state: &dyn ProtocolStorage, validator_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_addr(validator_id)))
}

/// Read validator stake from EVM storage.
pub fn read_validator_stake(evm_state: &dyn ProtocolStorage, addr: Address) -> u128 {
    u256_to_u128(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_stake(addr)))
}

/// Read validator ed25519 pubkey from EVM storage.
pub fn read_validator_pubkey(evm_state: &dyn ProtocolStorage, addr: Address) -> [u8; 32] {
    let pk_u256 = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_pubkey(addr));
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pk_u256.to_be_bytes::<32>());
    pk
}

/// Read validator status from EVM storage.
pub fn read_validator_status(evm_state: &dyn ProtocolStorage, addr: Address) -> u8 {
    evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_status(addr)).to_be_bytes::<32>()[31]
}

/// Read validator BLS pubkey from EVM storage.
pub fn read_validator_bls_pubkey(evm_state: &dyn ProtocolStorage, addr: Address) -> [u8; 48] {
    let hi = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_bls_pubkey(addr));
    let lo = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_bls_pubkey(addr) + U256::from(1));
    let mut pk = [0u8; 48];
    pk[0..32].copy_from_slice(&hi.to_be_bytes::<32>());
    pk[32..48].copy_from_slice(&lo.to_be_bytes::<32>()[0..16]);
    pk
}

/// Set validator BLS pubkey in EVM storage.
pub fn set_validator_bls_pubkey(evm_state: &mut dyn ProtocolStorage, addr: Address, bls_pubkey: [u8; 48]) {
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_validator_bls_pubkey(addr),
        U256::from_be_slice(&bls_pubkey[0..32]),
    );
    let mut lo_bytes = [0u8; 32];
    lo_bytes[0..16].copy_from_slice(&bls_pubkey[32..48]);
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_validator_bls_pubkey(addr) + U256::from(1),
        U256::from_be_slice(&lo_bytes),
    );
}

/// Read validator ID by address from EVM storage.
pub fn read_validator_id_by_addr(evm_state: &dyn ProtocolStorage, addr: Address) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(addr)))
}

/// Read all validator IDs and their stakes from EVM storage.
pub fn read_validators(evm_state: &dyn ProtocolStorage) -> Vec<(u64, Address, u128)> {
    let count = read_validator_count(evm_state);
    let mut out = Vec::new();
    for id in 1..=count {
        let addr = read_validator_addr(evm_state, id);
        if addr != Address::ZERO {
            let stake = read_validator_stake(evm_state, addr);
            out.push((id, addr, stake));
        }
    }
    out
}

/// Read all active validator addresses from EVM storage.
pub fn read_validator_addresses(evm_state: &dyn ProtocolStorage) -> Vec<Address> {
    let count = read_validator_count(evm_state);
    let mut addrs = Vec::new();
    for i in 1..=count {
        let addr = read_validator_addr(evm_state, i);
        if addr != Address::ZERO && read_validator_status(evm_state, addr) != 0 {
            addrs.push(addr);
        }
    }
    addrs
}

// ── Governance read helpers ───────────────────────────────────────────

/// Read governance proposal status from EVM storage.
pub fn read_gov_proposal_status(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u8 {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status")).to_be_bytes::<32>()[31]
}

/// Read governance paused flag from EVM storage.
pub fn read_gov_paused(evm_state: &dyn ProtocolStorage) -> bool {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_paused()).to_be_bytes::<32>()[31] == 1
}

/// Read governance proposal count from EVM storage.
pub fn read_gov_proposal_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal_count()))
}

/// Read governance proposal proposer from EVM storage.
pub fn read_gov_proposal_proposer(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"proposer")))
}

/// Read governance proposal title bytes from EVM storage.
pub fn read_gov_proposal_title(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> [u8; 32] {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"title")).to_be_bytes::<32>()
}

/// Read governance proposal description bytes from EVM storage.
pub fn read_gov_proposal_description(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> [u8; 32] {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"desc")).to_be_bytes::<32>()
}

/// Read governance proposal data hash from EVM storage.
pub fn read_gov_proposal_data_hash(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> [u8; 32] {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"data")).to_be_bytes::<32>()
}

/// Read governance proposal execution data from chunked EVM storage.
/// Mirrors GovernanceStorage::read_proposal_execution_data in the precompile.
pub fn read_gov_proposal_execution_data(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> Vec<u8> {
    let data_len = u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal_data_len(proposal_id))) as usize;
    if data_len == 0 {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(data_len);
    let num_chunks = (data_len + 31) / 32;
    for chunk_idx in 0..num_chunks {
        let chunk = evm_state
            .get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal_data_chunk(proposal_id, chunk_idx as u64))
            .to_be_bytes::<32>();
        let remaining = data_len - result.len();
        result.extend_from_slice(&chunk[..remaining.min(32)]);
    }
    result
}

/// Read governance proposal votes from EVM storage.
pub fn read_gov_proposal_votes(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> (u128, u128, u128) {
    let for_votes = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_for")));
    let against = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_against")));
    let abstain = u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"votes_abstain")));
    (for_votes, against, abstain)
}

/// Read governance proposal deposit from EVM storage.
pub fn read_gov_proposal_deposit(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"deposit")))
}

/// Read governance proposal queued_at block from EVM storage.
pub fn read_gov_proposal_queued_at(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"queued_at")))
}

/// Read whether a specific voter has voted on a proposal.
pub fn read_gov_voter_vote(evm_state: &dyn ProtocolStorage, proposal_id: u64, voter: Address) -> u8 {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_voter(proposal_id, voter)).to_be_bytes::<32>()[31]
}

/// Read governance proposal start_block from EVM storage.
pub fn read_gov_proposal_start_block(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"start_block")))
}

/// Read governance proposal end_block from EVM storage.
pub fn read_gov_proposal_end_block(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"end_block")))
}

/// Read governance proposal execution_block from EVM storage.
pub fn read_gov_proposal_execution_block(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"execution_block")))
}

/// Read governance proposal type from EVM storage.
pub fn read_gov_proposal_type(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u8 {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"proposal_type")).to_be_bytes::<32>()[31]
}

/// Read governance proposal review period end block from EVM storage.
pub fn read_gov_proposal_review_end(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"review_end")))
}

/// Write governance proposal review period end block to EVM storage.
pub fn write_gov_proposal_review_end(evm_state: &mut dyn ProtocolStorage, proposal_id: u64, review_end: u64) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"review_end"), u64_to_u256(review_end));
}

/// Read governance proposal quorum required from EVM storage.
pub fn read_gov_proposal_quorum_required(evm_state: &dyn ProtocolStorage, proposal_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"quorum_required")))
}

/// Write governance proposal quorum required to EVM storage.
pub fn write_gov_proposal_quorum_required(evm_state: &mut dyn ProtocolStorage, proposal_id: u64, quorum: u128) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"quorum_required"), u128_to_u256(quorum));
}

/// Read last submission block for an address from EVM storage.
pub fn read_gov_last_submission_block(evm_state: &dyn ProtocolStorage, addr: Address) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_last_submission(addr)))
}

/// Write last submission block for an address to EVM storage.
pub fn write_gov_last_submission_block(evm_state: &mut dyn ProtocolStorage, addr: Address, block: u64) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_last_submission(addr), u64_to_u256(block));
}

fn slot_gov_last_submission(addr: Address) -> U256 {
    storage_slot(&[b"last_submit", addr.as_slice()])
}

// ── Governance config helpers ─────────────────────────────────────────

pub fn slot_gov_config(suffix: &[u8]) -> U256 {
    storage_slot(&[b"gov_config", suffix])
}

/// Read governance config voting_period from EVM storage.
pub fn read_gov_config_voting_period(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"voting_period")))
}

/// Read governance config timelock from EVM storage.
pub fn read_gov_config_timelock(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"timelock")))
}

/// Read governance config execution_timeout from EVM storage.
pub fn read_gov_config_execution_timeout(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"execution_timeout")))
}

/// Read governance config deposit from EVM storage.
pub fn read_gov_config_deposit(evm_state: &dyn ProtocolStorage) -> u128 {
    u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"deposit")))
}

/// Read governance config quorum_bps from EVM storage.
pub fn read_gov_config_quorum_bps(evm_state: &dyn ProtocolStorage) -> u128 {
    u256_to_u128(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"quorum_bps")))
}

/// Read governance config review_period from EVM storage.
pub fn read_gov_config_review_period(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"review_period")))
}

/// Read governance config validator_quorum_bps from EVM storage.
pub fn read_gov_config_validator_quorum_bps(evm_state: &dyn ProtocolStorage) -> u32 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"validator_quorum_bps"))) as u32
}

/// Read governance config supply_quorum_bps from EVM storage.
pub fn read_gov_config_supply_quorum_bps(evm_state: &dyn ProtocolStorage) -> u32 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"supply_quorum_bps"))) as u32
}

/// Read governance config treasury_quorum_bps from EVM storage.
pub fn read_gov_config_treasury_quorum_bps(evm_state: &dyn ProtocolStorage) -> u32 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"treasury_quorum_bps"))) as u32
}

/// Read governance config simple_majority_bps from EVM storage.
pub fn read_gov_config_simple_majority_bps(evm_state: &dyn ProtocolStorage) -> u32 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"simple_majority_bps"))) as u32
}

/// Read governance config proposal_cooldown from EVM storage.
pub fn read_gov_config_proposal_cooldown(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_config(b"proposal_cooldown")))
}

/// Write governance proposal status to EVM storage.
pub fn write_gov_proposal_status(evm_state: &mut dyn ProtocolStorage, proposal_id: u64, status: u8) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status"), U256::from(status));
}

/// Write a governance config value (u128) to EVM storage.
pub fn write_gov_config_u128(evm_state: &mut dyn ProtocolStorage, suffix: &[u8], value: u128) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(suffix), u128_to_u256(value));
}

/// Seed governance config defaults into EVM storage.
pub fn seed_gov_config(evm_state: &mut dyn ProtocolStorage) {
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"voting_period"), u64_to_u256(100));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"timelock"), u64_to_u256(100));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"execution_timeout"), u64_to_u256(1000));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"deposit"), u128_to_u256(10_000));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"quorum_bps"), u128_to_u256(3_333));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"review_period"), u64_to_u256(10));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"validator_quorum_bps"), u64_to_u256(6667));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"supply_quorum_bps"), u64_to_u256(2000));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"treasury_quorum_bps"), u64_to_u256(2000));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"simple_majority_bps"), u64_to_u256(5001));
    evm_state.set_storage(GOVERNANCE_ADDRESS, slot_gov_config(b"proposal_cooldown"), u64_to_u256(345_600));
}

// ── Compliance read helpers ───────────────────────────────────────────

/// Read compliance status for an address under a policy from EVM storage.
pub fn read_compliance_status(evm_state: &dyn ProtocolStorage, addr: Address, policy_id: u8) -> u8 {
    evm_state.get_storage(&COMPLIANCE_ADDRESS, slot_compliance(addr, policy_id)).to_be_bytes::<32>()[31]
}

// ── Oracle reward pool (stored in EVM storage) ────────────────────────

fn slot_oracle_reward_pool() -> U256 {
    storage_slot(&[b"oracle_reward_pool"])
}

/// Read the oracle reward pool from EVM storage.
pub fn read_oracle_reward_pool(evm_state: &dyn ProtocolStorage) -> u128 {
    evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle_reward_pool())
        .to_be_bytes::<32>()[16..32]
        .try_into()
        .map(u128::from_be_bytes)
        .unwrap_or(0)
}

/// Add to the oracle reward pool in EVM storage.
pub fn add_oracle_reward(evm_state: &mut dyn ProtocolStorage, amount: u128) {
    let current = read_oracle_reward_pool(evm_state);
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_reward_pool(), u128_to_u256(current + amount));
}

// ── Oracle price read helpers ─────────────────────────────────────────

/// Read oracle price for an asset from EVM storage.
pub fn read_oracle_price(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle(asset_id, b"price")))
}

/// Read oracle TWAP for an asset from EVM storage.
pub fn read_oracle_twap(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle(asset_id, b"twap")))
}

/// Read oracle timestamp for an asset from EVM storage.
pub fn read_oracle_timestamp(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle(asset_id, b"ts")))
}

/// Read oracle block number for an asset from EVM storage.
pub fn read_oracle_block(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle(asset_id, b"block")))
}

/// Read oracle submission count for an asset from EVM storage.
pub fn read_oracle_count(evm_state: &dyn ProtocolStorage, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle(asset_id, b"count")))
}

/// Write oracle price data to EVM storage (used by protocol layer after quorum aggregation).
pub fn seed_oracle_price(
    evm_state: &mut dyn ProtocolStorage,
    asset_id: u64,
    price: u128,
    twap: u128,
    timestamp: u64,
    block: u64,
    count: u64,
) {
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"price"), u128_to_u256(price));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"twap"), u128_to_u256(twap));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"ts"), u64_to_u256(timestamp));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"block"), u64_to_u256(block));
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle(asset_id, b"count"), u64_to_u256(count));
}

// ── Oracle tracked assets ─────────────────────────────────────────────

fn slot_oracle_tracked_count() -> U256 {
    storage_slot(&[b"tracked_count"])
}

fn slot_oracle_tracked_asset(index: u64) -> U256 {
    storage_slot(&[b"tracked", &index.to_be_bytes()[..]])
}

/// Read the number of tracked oracle assets from EVM storage.
pub fn read_oracle_tracked_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle_tracked_count()))
}

/// Read a tracked oracle asset ID by index from EVM storage.
pub fn read_oracle_tracked_asset(evm_state: &dyn ProtocolStorage, index: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle_tracked_asset(index)))
}

/// Set the list of tracked oracle assets in EVM storage.
pub fn seed_oracle_tracked_assets(evm_state: &mut dyn ProtocolStorage, asset_ids: Vec<u64>) {
    let count = asset_ids.len() as u64;
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_tracked_count(), u64_to_u256(count));
    for (i, asset_id) in asset_ids.iter().enumerate() {
        evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_tracked_asset(i as u64), u64_to_u256(*asset_id));
    }
    // Zero out any old entries beyond the new list
    let old_count = read_oracle_tracked_count(evm_state);
    for i in asset_ids.len() as u64..old_count {
        evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_tracked_asset(i), U256::ZERO);
    }
}

/// Zero out the oracle reward pool in EVM storage.
pub fn zero_oracle_reward_pool(evm_state: &mut dyn ProtocolStorage) {
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_reward_pool(), U256::ZERO);
}

/// Seed validator state directly into EVM storage (for tests / genesis).
pub fn seed_validator(
    evm_state: &mut dyn ProtocolStorage,
    validator_id: u64,
    addr: Address,
    ed25519_pubkey: [u8; 32],
    stake: u128,
    status: u8,
) {
    let count = u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()));
    if validator_id > count {
        evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_count(), u64_to_u256(validator_id));
    }
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_addr(validator_id), address_to_u256(addr));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_by_addr(addr), u64_to_u256(validator_id));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(stake));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_pubkey(addr), U256::from_be_slice(&ed25519_pubkey));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(addr), U256::from(status));
    set_validator_bls_pubkey(evm_state, addr, [0u8; 48]);
}

/// Remove a validator from EVM storage (set stake to 0 and status to 0).
pub fn remove_validator_evm(evm_state: &mut dyn ProtocolStorage, addr: Address) {
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), U256::ZERO);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(addr), U256::ZERO);
}

/// Rotate a validator's ed25519 pubkey in EVM storage.
pub fn rotate_validator_key_evm(
    evm_state: &mut dyn ProtocolStorage,
    addr: Address,
    new_ed25519_pubkey: [u8; 32],
) {
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_validator_pubkey(addr),
        U256::from_be_slice(&new_ed25519_pubkey),
    );
}

/// Stake a new validator directly into EVM storage.
/// Returns the assigned validator_id.
pub fn stake_validator_evm(
    evm_state: &mut dyn ProtocolStorage,
    addr: Address,
    ed25519_pubkey: [u8; 32],
    amount: u128,
) -> u64 {
    let count = read_validator_count(evm_state);
    let validator_id = count + 1;
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_count(), u64_to_u256(validator_id));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_addr(validator_id), address_to_u256(addr));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_by_addr(addr), u64_to_u256(validator_id));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(amount));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_pubkey(addr), U256::from_be_slice(&ed25519_pubkey));
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(addr), U256::from(1u8));
    set_validator_bls_pubkey(evm_state, addr, [0u8; 48]);
    validator_id
}

/// Slash a validator's stake in EVM storage.
/// Returns the amount actually slashed.
pub fn slash_validator_evm(
    evm_state: &mut dyn ProtocolStorage,
    addr: Address,
    amount: u128,
) -> u128 {
    let current = read_validator_stake(evm_state, addr);
    let slashed = current.min(amount);
    let new_stake = current.saturating_sub(slashed);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(new_stake));
    if new_stake == 0 {
        evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_status(addr), U256::ZERO);
    }
    slashed
}

/// Distribute a reward to a validator by adding to their stake in EVM storage.
pub fn distribute_reward_evm(
    evm_state: &mut dyn ProtocolStorage,
    addr: Address,
    amount: u128,
) {
    let current = read_validator_stake(evm_state, addr);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(current + amount));
}

/// Seed agent metadata directly into EVM storage (for tests / genesis).
pub fn seed_agent(
    evm_state: &mut dyn ProtocolStorage,
    agent_id: u64,
    owner: Address,
    name: &str,
    url: &str,
    registered_at: u64,
) {
    let count = u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_count()));
    if agent_id >= count {
        evm_state.set_storage(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(agent_id + 1));
    }
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_owner(agent_id), address_to_u256(owner));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_name(agent_id), write_string32(name));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_url(agent_id), write_string32(url));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_registered_at(agent_id), u64_to_u256(registered_at));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_perms(agent_id), pack_agent_perms(1_000, 0, 1));
}

// ── Shielded storage slot helpers ─────────────────────────────────────

fn slot_shielded_merkle_root() -> U256 {
    storage_slot(&[b"merkle_root"])
}

fn slot_shielded_nullifier(nullifier: &call_shielded::Nullifier) -> U256 {
    storage_slot(&[b"nullifier", nullifier.as_ref()])
}

fn slot_shielded_commitment_count() -> U256 {
    storage_slot(&[b"cm_count"])
}

fn slot_shielded_commitment(index: u64) -> U256 {
    storage_slot(&[b"commitment", &index.to_be_bytes()[..]])
}

// ── Shielded read helpers ─────────────────────────────────────────────

/// Read shielded merkle root from EVM storage.
pub fn read_shielded_merkle_root(evm_state: &dyn ProtocolStorage) -> call_primitives::Hash {
    let root_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_merkle_root());
    call_primitives::Hash::from_slice(&root_u256.to_be_bytes::<32>())
}

/// Read shielded commitment count from EVM storage.
pub fn read_shielded_commitment_count(evm_state: &dyn ProtocolStorage) -> u64 {
    u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()))
}

/// Read a shielded commitment by index from EVM storage.
pub fn read_shielded_commitment(evm_state: &dyn ProtocolStorage, index: u64) -> call_primitives::Hash {
    let cm_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment(index));
    call_primitives::Hash::from_slice(&cm_u256.to_be_bytes::<32>())
}

/// Check if a nullifier is spent in EVM storage.
pub fn read_shielded_nullifier_spent(evm_state: &dyn ProtocolStorage, nullifier: &call_shielded::Nullifier) -> bool {
    evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_nullifier(nullifier)).to_be_bytes::<32>()[31] == 1
}

/// Seed a shielded commitment directly into EVM storage (for tests / genesis).
pub fn seed_shielded_commitment(
    evm_state: &mut dyn ProtocolStorage,
    index: u64,
    commitment: call_primitives::Hash,
) {
    let count = u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()));
    if index >= count {
        evm_state.set_storage(SHIELDED_ADDRESS, slot_shielded_commitment_count(), u64_to_u256(index + 1));
    }
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_commitment(index),
        U256::from_be_slice(commitment.as_slice()),
    );
}

/// Seed a shielded nullifier as spent directly into EVM storage (for tests / genesis).
pub fn seed_shielded_nullifier(evm_state: &mut dyn ProtocolStorage, nullifier: &call_shielded::Nullifier) {
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_nullifier(nullifier),
        U256::from(1),
    );
}

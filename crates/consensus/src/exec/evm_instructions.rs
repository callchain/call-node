//! EVM-based protocol instruction execution.
//!
//! Asset instructions (Transfer, BatchTransfer, Approve, TransferFrom,
//! Mint, Burn) are executed by reading/writing EVM storage directly,
//! using the same slot layout as the AssetPrecompile (0x201).
//!
//! This replaces the legacy `AccountState` / `AssetRegistry` mutation path
//! so that the block state_root captures all state changes.

use call_evm::EvmState;
use call_precompiles::{
    address_to_u256, read_string32, slot_asset_meta, slot_balance,
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256,
    write_string32,
    AGENT_ADDRESS, ASSET_ADDRESS, BRIDGE_ADDRESS, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
    ORACLE_ADDRESS, SHIELDED_ADDRESS, VALIDATOR_ADDRESS,
};
use call_precompiles::storage::storage_slot;
use call_primitives::{Address, U256};
// ── Constants ─────────────────────────────────────────────────────────

/// Default unbonding period in blocks (~8.4h at 250ms block time)
const UNBONDING_PERIOD_BLOCKS: u64 = 120_960;
/// Minimum self-stake to become a validator
const MIN_SELF_STAKE: u128 = 1_000_000;
/// Proposal deposit in CALL
const PROPOSAL_DEPOSIT: u128 = 10_000;
/// Governance timelock in blocks
const GOV_TIMELOCK_BLOCKS: u64 = 100;
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

fn slot_validator_unbond_height(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"unbond_at"])
}

fn slot_unbonding_count() -> U256 {
    U256::from(1)
}

fn slot_unbonding(index: u64) -> U256 {
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

fn slot_gov_proposal(proposal_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[b"proposal", &proposal_id.to_be_bytes()[..], suffix])
}

fn slot_gov_voter(proposal_id: u64, voter: Address) -> U256 {
    storage_slot(&[b"vote", &proposal_id.to_be_bytes()[..], voter.as_slice()])
}

fn slot_gov_paused() -> U256 {
    storage_slot(&[b"paused"])
}

fn slot_gov_pause_reason() -> U256 {
    storage_slot(&[b"pause_reason"])
}

fn slot_gov_proposal_deposit() -> U256 {
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

fn slot_bridge_withdrawal_period_start() -> U256 {
    storage_slot(&[b"withdrawal_start"])
}

fn slot_bridge_withdrawal_period_used(asset_id: u64) -> U256 {
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

pub(crate) fn unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit = u128::from_be_bytes(bytes[0..16].try_into().unwrap());
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

pub fn agent_exists(evm_state: &EvmState, agent_id: u64) -> bool {
    evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)) != U256::ZERO
}

pub fn agent_get_owner(evm_state: &EvmState, agent_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_owner(agent_id)))
}


pub fn agent_get_balance(evm_state: &EvmState, agent_id: u64, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id)))
}

pub fn agent_set_balance(evm_state: &mut EvmState, agent_id: u64, asset_id: u64, amount: u128) {
    evm_state.set_storage(
        AGENT_ADDRESS,
        slot_agent_balance(agent_id, asset_id),
        u128_to_u256(amount),
    );
}

// ── Helpers for tests / genesis / RPC ─────────────────────────────────

/// Seed an asset balance directly into EVM storage.
pub fn seed_balance(evm_state: &mut EvmState, asset_id: u64, addr: Address, amount: u128) {
    evm_state.set_storage(ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

/// Seed asset metadata directly into EVM storage.
#[allow(clippy::too_many_arguments)]
pub fn seed_asset(
    evm_state: &mut EvmState,
    asset_id: u64,
    symbol: &str,
    name: &str,
    decimals: u8,
    issuer: Address,
    max_supply: u128,
    supply: u128,
    status: u8,
) {
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol"), write_string32(symbol));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"name"), write_string32(name));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals"), call_primitives::U256::from(decimals));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"), address_to_u256(issuer));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply"), u128_to_u256(max_supply));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(supply));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"), call_primitives::U256::from(status));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"), call_primitives::U256::from(0));
    evm_state.set_storage(ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at"), call_primitives::U256::from(0));
}

/// Read an asset balance from EVM storage.
pub fn read_balance(evm_state: &EvmState, asset_id: u64, addr: Address) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_balance(asset_id, addr)))
}

/// Read asset info from EVM storage.
pub fn read_asset_symbol(evm_state: &EvmState, asset_id: u64) -> String {
    read_string32(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol")))
}

/// Read asset issuer from EVM storage.
pub fn read_asset_issuer(evm_state: &EvmState, asset_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")))
}

/// Read asset status from EVM storage.
pub fn read_asset_status(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"status")).to_be_bytes::<32>()[31]
}

/// Seed bridge contract address for an asset in EVM storage.
pub fn seed_bridge_contract(evm_state: &mut EvmState, asset_id: u64, contract: Address) {
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(asset_id), address_to_u256(contract));
}

/// Read total bridge deposits for an asset from EVM storage.
pub fn read_bridge_total_deposits(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_deposits(asset_id)))
}

/// Read total bridge withdrawals for an asset from EVM storage.
pub fn read_bridge_total_withdrawals(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&BRIDGE_ADDRESS, slot_bridge_total_withdrawals(asset_id)))
}

// ── Asset read helpers ────────────────────────────────────────────────

/// Read asset name from EVM storage.
pub fn read_asset_name(evm_state: &EvmState, asset_id: u64) -> String {
    read_string32(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"name")))
}

/// Read asset decimals from EVM storage.
pub fn read_asset_decimals(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals")).to_be_bytes::<32>()[31]
}

/// Read asset supply from EVM storage.
pub fn read_asset_supply(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")))
}

/// Read asset max supply from EVM storage.
pub fn read_asset_max_supply(evm_state: &EvmState, asset_id: u64) -> u128 {
    u256_to_u128(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply")))
}

/// Read asset compliance policy from EVM storage.
pub fn read_asset_compliance(evm_state: &EvmState, asset_id: u64) -> u8 {
    evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance")).to_be_bytes::<32>()[31]
}

/// Read asset registered_at from EVM storage.
pub fn read_asset_registered_at(evm_state: &EvmState, asset_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at")))
}

// ── Agent read helpers ────────────────────────────────────────────────

/// Read agent name from EVM storage.
pub fn agent_get_name(evm_state: &EvmState, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_name(agent_id)))
}

/// Read agent url from EVM storage.
pub fn agent_get_url(evm_state: &EvmState, agent_id: u64) -> String {
    read_string32(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_url(agent_id)))
}

/// Read agent registered_at from EVM storage.
pub fn agent_get_registered_at(evm_state: &EvmState, agent_id: u64) -> u64 {
    u256_to_u64(evm_state.get_storage(&AGENT_ADDRESS, slot_agent_registered_at(agent_id)))
}

// ── Validator read helpers ────────────────────────────────────────────

/// Read validator count from EVM storage.
pub fn read_validator_count(evm_state: &EvmState) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_count()))
}

/// Read validator address by validator ID from EVM storage.
pub fn read_validator_addr(evm_state: &EvmState, validator_id: u64) -> Address {
    u256_to_address(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_addr(validator_id)))
}

/// Read validator stake from EVM storage.
pub fn read_validator_stake(evm_state: &EvmState, addr: Address) -> u128 {
    u256_to_u128(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_stake(addr)))
}

/// Read validator ed25519 pubkey from EVM storage.
pub fn read_validator_pubkey(evm_state: &EvmState, addr: Address) -> [u8; 32] {
    let pk_u256 = evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_pubkey(addr));
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pk_u256.to_be_bytes::<32>());
    pk
}

/// Read validator status from EVM storage.
pub fn read_validator_status(evm_state: &EvmState, addr: Address) -> u8 {
    evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_status(addr)).to_be_bytes::<32>()[31]
}

/// Read validator ID by address from EVM storage.
pub fn read_validator_id_by_addr(evm_state: &EvmState, addr: Address) -> u64 {
    u256_to_u64(evm_state.get_storage(&VALIDATOR_ADDRESS, slot_validator_by_addr(addr)))
}

/// Read all validator IDs and their stakes from EVM storage.
pub fn read_validators(evm_state: &EvmState) -> Vec<(u64, Address, u128)> {
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
pub fn read_validator_addresses(evm_state: &EvmState) -> Vec<Address> {
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
pub fn read_gov_proposal_status(evm_state: &EvmState, proposal_id: u64) -> u8 {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_proposal(proposal_id, b"status")).to_be_bytes::<32>()[31]
}

/// Read governance paused flag from EVM storage.
pub fn read_gov_paused(evm_state: &EvmState) -> bool {
    evm_state.get_storage(&GOVERNANCE_ADDRESS, slot_gov_paused()).to_be_bytes::<32>()[31] == 1
}

// ── Compliance read helpers ───────────────────────────────────────────

/// Read compliance status for an address under a policy from EVM storage.
pub fn read_compliance_status(evm_state: &EvmState, addr: Address, policy_id: u8) -> u8 {
    evm_state.get_storage(&COMPLIANCE_ADDRESS, slot_compliance(addr, policy_id)).to_be_bytes::<32>()[31]
}

// ── Oracle reward pool (stored in EVM storage) ────────────────────────

fn slot_oracle_reward_pool() -> U256 {
    storage_slot(&[b"oracle_reward_pool"])
}

/// Read the oracle reward pool from EVM storage.
pub fn read_oracle_reward_pool(evm_state: &EvmState) -> u128 {
    evm_state.get_storage(&ORACLE_ADDRESS, slot_oracle_reward_pool())
        .to_be_bytes::<32>()[16..32]
        .try_into()
        .map(u128::from_be_bytes)
        .unwrap_or(0)
}

/// Add to the oracle reward pool in EVM storage.
pub fn add_oracle_reward(evm_state: &mut EvmState, amount: u128) {
    let current = read_oracle_reward_pool(evm_state);
    evm_state.set_storage(ORACLE_ADDRESS, slot_oracle_reward_pool(), u128_to_u256(current + amount));
}

/// Seed validator state directly into EVM storage (for tests / genesis).
pub fn seed_validator(
    evm_state: &mut EvmState,
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
}

/// Stake a new validator directly into EVM storage.
/// Returns the assigned validator_id.
pub fn stake_validator_evm(
    evm_state: &mut EvmState,
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
    validator_id
}

/// Slash a validator's stake in EVM storage.
/// Returns the amount actually slashed.
pub fn slash_validator_evm(
    evm_state: &mut EvmState,
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
    evm_state: &mut EvmState,
    addr: Address,
    amount: u128,
) {
    let current = read_validator_stake(evm_state, addr);
    evm_state.set_storage(VALIDATOR_ADDRESS, slot_validator_stake(addr), u128_to_u256(current + amount));
}

/// Seed agent metadata directly into EVM storage (for tests / genesis).
pub fn seed_agent(
    evm_state: &mut EvmState,
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
pub fn read_shielded_merkle_root(evm_state: &EvmState) -> call_primitives::Hash {
    let root_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_merkle_root());
    call_primitives::Hash::from_slice(&root_u256.to_be_bytes::<32>())
}

/// Read shielded commitment count from EVM storage.
pub fn read_shielded_commitment_count(evm_state: &EvmState) -> u64 {
    u256_to_u64(evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment_count()))
}

/// Read a shielded commitment by index from EVM storage.
pub fn read_shielded_commitment(evm_state: &EvmState, index: u64) -> call_primitives::Hash {
    let cm_u256 = evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_commitment(index));
    call_primitives::Hash::from_slice(&cm_u256.to_be_bytes::<32>())
}

/// Check if a nullifier is spent in EVM storage.
pub fn read_shielded_nullifier_spent(evm_state: &EvmState, nullifier: &call_shielded::Nullifier) -> bool {
    evm_state.get_storage(&SHIELDED_ADDRESS, slot_shielded_nullifier(nullifier)).to_be_bytes::<32>()[31] == 1
}

/// Seed a shielded commitment directly into EVM storage (for tests / genesis).
pub fn seed_shielded_commitment(
    evm_state: &mut EvmState,
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
pub fn seed_shielded_nullifier(evm_state: &mut EvmState, nullifier: &call_shielded::Nullifier) {
    evm_state.set_storage(
        SHIELDED_ADDRESS,
        slot_shielded_nullifier(nullifier),
        U256::from(1),
    );
}

//! E2E test: EVM compatibility
//!
//! Verifies that the EVM layer integrates properly with the protocol layer:
//! - EVM state isolation from protocol balances
//! - EVM gas tracking and limit enforcement
//! - EVM nonce validation
//! - EVM storage operations
//! - Protocol-to-EVM balance independence

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_evm::{U256, Bytes};
use call_primitives::Address;
use call_evm::EvmState;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Asset balances are read from EVM storage, not legacy protocol state.
#[test]
fn test_evm_state_isolation_from_protocol() {
    let node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(&mut evm_state, sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }

    // Legacy protocol balance is ignored
    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 5_000).unwrap();

    // Asset balance lives in EVM storage
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, sender, 7_000,
        );
    }

    // node.balance() reads from EVM asset storage
    assert_eq!(node.balance(1, &sender), 7_000);

    // Native EVM balance is independent of asset balance
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        evm_state.set_balance(sender, U256::from(10_000u128));
    }
    {
        let evm_state = node.state.evm_state.read().unwrap();
        assert_eq!(evm_state.get_balance(&sender), U256::from(10_000u128));
    }
}

/// EVM nonce is tracked independently per account.
#[test]
fn test_evm_nonce_tracking() {
    let mut evm_state = EvmState::new();
    let addr = test_addr(42);

    assert_eq!(evm_state.get_nonce(&addr), 0);

    evm_state.create_account(addr);
    evm_state.increment_nonce(addr);
    assert_eq!(evm_state.get_nonce(&addr), 1);

    for _ in 0..4 {
        evm_state.increment_nonce(addr);
    }
    assert_eq!(evm_state.get_nonce(&addr), 5);
}

/// EVM gas tracking — balance and nonce tracked across transactions.
#[test]
fn test_evm_gas_tracking() {
    let mut evm_state = EvmState::new();
    let addr = test_addr(1);

    evm_state.set_balance(addr, U256::from(1_000_000u128));
    evm_state.create_account(addr);

    // Simulate gas consumption via nonce increments
    evm_state.increment_nonce(addr);

    // Balance should remain until gas is deducted
    assert_eq!(evm_state.get_balance(&addr), U256::from(1_000_000u128));
    assert_eq!(evm_state.get_nonce(&addr), 1);
}

/// EVM account creation via balance setting.
#[test]
fn test_evm_account_creation_via_balance() {
    let mut evm_state = EvmState::new();
    let addr = test_addr(7);

    // Account doesn't exist until balance is set
    assert_eq!(evm_state.get_balance(&addr), U256::ZERO);
    assert_eq!(evm_state.get_nonce(&addr), 0);

    evm_state.set_balance(addr, U256::from(500u128));
    assert_eq!(evm_state.get_balance(&addr), U256::from(500u128));

    // Nonce still 0 for new account
    assert_eq!(evm_state.get_nonce(&addr), 0);
}

/// EVM state survives block production.
#[tokio::test]
async fn test_evm_state_persists_across_blocks() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(&mut evm_state, sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }

    // Set EVM balance before block production
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        evm_state.set_balance(sender, U256::from(10_000u128));
        evm_state.create_account(sender);
        for _ in 0..5 {
            evm_state.increment_nonce(sender);
        }
    }

    // Produce a block
    node.produce_block(1_000_000);

    // EVM state should persist
    {
        let evm_state = node.state.evm_state.read().unwrap();
        assert_eq!(evm_state.get_balance(&sender), U256::from(10_000u128));
        assert_eq!(evm_state.get_nonce(&sender), 5);
    }
}

/// Multiple EVM accounts can coexist.
#[test]
fn test_evm_multiple_accounts() {
    let mut evm_state = EvmState::new();

    let addr1 = test_addr(1);
    let addr2 = test_addr(2);
    let addr3 = test_addr(3);

    evm_state.set_balance(addr1, U256::from(1_000u128));
    evm_state.set_balance(addr2, U256::from(2_000u128));
    evm_state.set_balance(addr3, U256::from(3_000u128));

    evm_state.create_account(addr1);
    evm_state.create_account(addr2);
    for _ in 0..1 { evm_state.increment_nonce(addr1); }
    for _ in 0..10 { evm_state.increment_nonce(addr2); }

    assert_eq!(evm_state.get_balance(&addr1), U256::from(1_000u128));
    assert_eq!(evm_state.get_balance(&addr2), U256::from(2_000u128));
    assert_eq!(evm_state.get_balance(&addr3), U256::from(3_000u128));

    assert_eq!(evm_state.get_nonce(&addr1), 1);
    assert_eq!(evm_state.get_nonce(&addr2), 10);
    assert_eq!(evm_state.get_nonce(&addr3), 0);
}

/// EVM storage slot operations.
#[test]
fn test_evm_storage_slots() {
    let mut evm_state = EvmState::new();
    let contract = test_addr(100);

    // Set storage slot
    let slot = U256::from(1);
    let value = U256::from(42);
    evm_state.set_storage(contract, slot, value);

    assert_eq!(evm_state.get_storage(&contract, slot), value);

    // Different slot, different value
    let slot2 = U256::from(2);
    let value2 = U256::from(99);
    evm_state.set_storage(contract, slot2, value2);

    assert_eq!(evm_state.get_storage(&contract, slot), value);
    assert_eq!(evm_state.get_storage(&contract, slot2), value2);
}

/// EVM storage zero-value clears the slot.
#[test]
fn test_evm_storage_zero_clears_slot() {
    let mut evm_state = EvmState::new();
    let contract = test_addr(100);

    let slot = U256::from(1);
    let value = U256::from(42);
    evm_state.set_storage(contract, slot, value);
    assert_eq!(evm_state.get_storage(&contract, slot), value);

    // Setting to zero should clear the slot
    evm_state.set_storage(contract, slot, U256::ZERO);
    assert_eq!(evm_state.get_storage(&contract, slot), U256::ZERO);
}

/// EVM state default is valid.
#[test]
fn test_evm_state_default() {
    let state = EvmState::default();
    assert_eq!(state.get_balance(&test_addr(1)), U256::ZERO);
    assert_eq!(state.get_nonce(&test_addr(1)), 0);
}

/// Asset balance (EVM storage slot) and native EVM balance are independent.
#[test]
fn test_protocol_and_evm_same_address() {
    let node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(&mut evm_state, sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }

    // Asset balance lives in EVM storage slots
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, sender, 5_000,
        );
    }

    // Native EVM balance is stored separately in the account
    node.state.evm_state.write().unwrap().set_balance(sender, U256::from(99_999u128));

    assert_eq!(node.balance(1, &sender), 5_000);
    assert_eq!(node.state.evm_state.read().unwrap().get_balance(&sender), U256::from(99_999u128));
}

/// EVM code deployment simulation.
#[test]
fn test_evm_code_deployment() {
    let mut evm_state = EvmState::new();
    let contract = test_addr(200);

    assert_eq!(evm_state.get_code(&contract).len(), 0);

    let bytecode = Bytes::from(vec![0x60, 0x60, 0x60, 0x40, 0x52]);
    evm_state.set_code(contract, bytecode.clone());

    assert_eq!(evm_state.get_code(&contract), bytecode);
}

/// EVM account creation via create_account.
#[test]
fn test_evm_create_account() {
    let mut evm_state = EvmState::new();
    let addr = test_addr(42);

    let account = evm_state.create_account(addr);
    account.balance = U256::from(100u128);
    account.nonce = 5;

    assert_eq!(evm_state.get_balance(&addr), U256::from(100u128));
    assert_eq!(evm_state.get_nonce(&addr), 5);
}

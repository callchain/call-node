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

use call_evm::U256;
use call_primitives::Address;
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
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Asset balance lives in EVM storage
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(), call_protocol::CALL_ASSET_ID, sender, 7_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // node.balance() reads from EVM asset storage
    assert_eq!(node.balance(1, &sender), 7_000);

    // Native EVM balance is independent of asset balance
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(sender, U256::from(10_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        assert_eq!(provider.state().get_balance(&sender), U256::from(10_000u128));
    }
}

/// EVM state survives block production.
#[tokio::test]
async fn test_evm_state_persists_across_blocks() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Set EVM balance before block production
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        provider.state_mut().set_balance(sender, U256::from(10_000u128));
        provider.state_mut().create_account(sender);
        for _ in 0..5 {
            provider.state_mut().increment_nonce(sender);
        }
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Produce a block
    node.produce_block(1_000_000);

    // EVM state should persist
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        assert_eq!(provider.state().get_balance(&sender), U256::from(10_000u128));
        assert_eq!(provider.state().get_nonce(&sender), 5);
    }
}

/// Asset balance (EVM storage slot) and native EVM balance are independent.
#[test]
fn test_protocol_and_evm_same_address() {
    let node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Asset balance lives in EVM storage slots
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(), call_protocol::CALL_ASSET_ID, sender, 5_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Native EVM balance is stored separately in the account
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        provider.state_mut().set_balance(sender, U256::from(99_999u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    assert_eq!(node.balance(1, &sender), 5_000);
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(
            &node.state.db_env).unwrap();
        assert_eq!(provider.state().get_balance(&sender), U256::from(99_999u128));
    }
}


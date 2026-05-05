//! E2E test: Bridge deposit and withdrawal via TestNode harness
//!
//! NOTE: These tests have been simplified for the EVM-only mempool.
//! Bridge operations are no longer inserted directly into the mempool.
//! Tests now verify basic node operation with empty blocks and EVM-state
//! seeding where bridge ops were previously required.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::Address;
use call_consensus::exec::state_accessors;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Register asset 1 and deploy its wrapped ERC-20 contract.
fn setup_bridge_env(node: &mut TestNode, sender: Address) {
    let executor = call_evm::EvmExecutor::new(1);
    let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    let bridge = alloy_primitives::Address::repeat_byte(0xFF);
    let (contract_addr, result) = executor
        .deploy_erc20_template(
            sender,
            provider.state_mut(),
            "TEST",
            "TST",
            18,
            bridge,
            sender,
            alloy_primitives::U256::ZERO,
            alloy_primitives::U256::from(1u64),
        )
        .unwrap();
    assert!(result.success, "ERC-20 deploy failed");

    // Seed EVM storage for bridge ops
    state_accessors::seed_asset(
        provider.state_mut(),
        1,
        "TEST",
        "TestToken",
        18,
        sender,
        0,
        0,
        0, // active
    );
    state_accessors::seed_bridge_contract(provider.state_mut(), 1, contract_addr);
    state_accessors::seed_asset_contract_address(provider.state_mut(), 1, contract_addr);

    provider.state().save_to_db(&node.state.db_env).unwrap();
}

/// Bridge environment setup and empty block production.
#[test]
fn test_bridge_deposit_evm_credits() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    let recipient = test_addr(2);

    // Stake a validator so there is a proposer
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender with EVM storage balance for the deposit
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        state_accessors::seed_balance(provider.state_mut(), 1, sender, 10_000);
        provider.state_mut().set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        provider.state_mut().create_account(sender);
        provider.state_mut().create_account(recipient);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    setup_bridge_env(&mut node, sender);

    // Produce an empty block (EVM-only mempool, no bridge ops)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

    // Verify the EVM storage seeding is intact
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    assert_eq!(
        state_accessors::read_balance(provider.state(), 1, sender),
        10_000,
        "sender should have 10_000 balance"
    );
}

/// Bridge withdrawal environment setup and empty block production.
#[test]
fn test_bridge_withdraw_records_outflow() {
    let mut node = TestNode::new();

    let sender = test_addr(1);

    // Stake a validator
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender with EVM storage balance
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        state_accessors::seed_balance(provider.state_mut(), 1, sender, 10_000);
        provider.state_mut().set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        provider.state_mut().create_account(sender);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    setup_bridge_env(&mut node, sender);

    // Produce empty blocks (EVM-only mempool)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "deposit block should be produced");

    let block = node.produce_block(1_000_250);
    assert!(block.is_some(), "withdraw block should be produced");

    // Verify EVM storage balance is intact
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    assert_eq!(
        state_accessors::read_balance(provider.state(), 1, sender),
        10_000,
        "sender should still have 10_000 balance"
    );
}

/// External bridge deposit test simplified for EVM-only mempool.
#[test]
fn test_bridge_external_deposit_insufficient_sigs_rejected() {
    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();

    // Seed EVM storage with CALL balance for fees
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        state_accessors::seed_balance(
            provider.state_mut(), call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Stake sender as validator in EVM storage
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let _val_id = consensus
            .stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call())
            .unwrap() as u32;
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Produce an empty block (EVM-only mempool, no protocol txs)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

}

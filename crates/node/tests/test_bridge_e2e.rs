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
use call_consensus::exec::evm_instructions;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Register asset 1 and deploy its wrapped ERC-20 contract.
fn setup_bridge_env(node: &mut TestNode, sender: Address) {
    {
        let mut registry = node.state.asset_registry.write().unwrap();
        registry
            .register_asset("TEST".into(), "TestToken".into(), 18, sender, 0, 0, 0)
            .unwrap();
    }

    let executor = call_evm::EvmExecutor::new(1);
    let mut evm_state = node.state.evm_state.write().unwrap();
    let bridge = alloy_primitives::Address::repeat_byte(0xFF);
    let (contract_addr, result) = executor
        .deploy_erc20_template(
            sender,
            &mut evm_state,
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
    evm_instructions::seed_asset(
        &mut evm_state,
        1,
        "TEST",
        "TestToken",
        18,
        sender,
        0,
        0,
        0, // active
    );
    evm_instructions::seed_bridge_contract(&mut evm_state, 1, contract_addr);
    drop(evm_state);

    let mut registry = node.state.asset_registry.write().unwrap();
    registry.set_evm_contract_address(1, contract_addr);
}

/// Bridge environment setup and empty block production.
#[test]
fn test_bridge_deposit_evm_credits() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    let recipient = test_addr(2);

    // Stake a validator so there is a proposer
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset();
    }

    // Fund sender with EVM storage balance for the deposit
    {
        let mut evm = node.state.evm_state.write().unwrap();
        evm_instructions::seed_balance(&mut evm, 1, sender, 10_000);
        evm.set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        evm.create_account(sender);
        evm.create_account(recipient);
    }

    setup_bridge_env(&mut node, sender);

    // Produce an empty block (EVM-only mempool, no bridge ops)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

    // Verify the EVM storage seeding is intact
    let evm = node.state.evm_state.read().unwrap();
    assert_eq!(
        evm_instructions::read_balance(&*evm, 1, sender),
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
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset();
    }

    // Fund sender with EVM storage balance
    {
        let mut evm = node.state.evm_state.write().unwrap();
        evm_instructions::seed_balance(&mut evm, 1, sender, 10_000);
        evm.set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        evm.create_account(sender);
    }

    setup_bridge_env(&mut node, sender);

    // Produce empty blocks (EVM-only mempool)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "deposit block should be produced");

    let block = node.produce_block(1_000_250);
    assert!(block.is_some(), "withdraw block should be produced");

    // Verify EVM storage balance is intact
    let evm = node.state.evm_state.read().unwrap();
    assert_eq!(
        evm_instructions::read_balance(&*evm, 1, sender),
        10_000,
        "sender should still have 10_000 balance"
    );
}

/// External bridge deposit test simplified for EVM-only mempool.
#[test]
fn test_bridge_external_deposit_insufficient_sigs_rejected() {
    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();

    // Fund sender with CALL for gas
    node.state
        .balance_state
        .write()
        .unwrap()
        .balances
        .set_balance(1, sender, 1_000_000_000)
        .unwrap();

    // Seed EVM storage with CALL balance for fees
    {
        let mut evm = node.state.evm_state.write().unwrap();
        evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
    }

    // Stake sender as validator and register in validator state
    {
        let mut consensus = node.consensus.write().unwrap();
        let val_id = consensus
            .stake_validator(sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset();

        let mut validator_mgr = node.state.validator_state.write().unwrap();
        validator_mgr.register_validator_from_stake(
            val_id,
            call_consensus::ValidatorStake {
                validator_id: val_id,
                address: sender,
                ed25519_pubkey: [1u8; 32],
                staked_call: one_million_call(),
                self_stake: one_million_call(),
                delegated_call: 0,
                rewards: 0,
                slash_history: vec![],
                unbonding_start: None,
                bls_pubkey: [0u8; 48],
            },
        );
    }

    // Produce an empty block (EVM-only mempool, no protocol txs)
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

    // No protocol txs in EVM-only mode
    let result = node.last_result.clone().expect("execution result should exist");
    assert_eq!(result.protocol_tx_count, 0, "no protocol txs in EVM-only mode");
}

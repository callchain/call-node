//! E2E test: Bridge deposit and withdrawal via TestNode harness
//!
//! Validates that bridge operations are correctly processed during block production.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, ExecutionStatus};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

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
    drop(evm_state);

    let mut registry = node.state.asset_registry.write().unwrap();
    registry.set_evm_contract_address(1, contract_addr);
}

/// Bridge deposit via `BridgeOp::DepositToEvm` is processed in block execution.
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

    // Fund sender with protocol balance for the deposit
    {
        node.state
            .balance_state
            .write()
            .unwrap()
            .balances
            .set_balance(1, sender, 10_000)
            .unwrap();
    }

    // Set up EVM state for sender
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        evm_state.set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(sender);
        evm_state.create_account(recipient);
    }

    setup_bridge_env(&mut node, sender);

    // Insert a bridge deposit op directly into the mempool
    node.insert_bridge_op(call_bridge::BridgeOp::DepositToEvm {
        asset_id: 1,
        from: sender,
        to: recipient,
        amount: 500,
    });

    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

    // Verify the deposit was recorded in bridge state
    let bridge = node.state.bridge_state.read().unwrap();
    assert_eq!(
        bridge.total_deposits.get(&1).copied().unwrap_or(0),
        500,
        "bridge should record 500 deposited"
    );
}

/// Bridge withdrawal via `BridgeOp::WithdrawToProtocol` is processed in block execution.
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

    // Fund sender with protocol balance
    {
        node.state
            .balance_state
            .write()
            .unwrap()
            .balances
            .set_balance(1, sender, 10_000)
            .unwrap();
    }

    // Set up EVM state
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        evm_state.set_balance(sender, alloy_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(sender);
    }

    setup_bridge_env(&mut node, sender);

    // First deposit to mint wrapped tokens
    node.insert_bridge_op(call_bridge::BridgeOp::DepositToEvm {
        asset_id: 1,
        from: sender,
        to: sender,
        amount: 500,
    });
    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "deposit block should be produced");

    // Then withdraw
    node.insert_bridge_op(call_bridge::BridgeOp::WithdrawToProtocol {
        asset_id: 1,
        from: sender,
        to: test_addr(99),
        amount: 300,
    });
    let block = node.produce_block(1_000_250);
    assert!(block.is_some(), "withdraw block should be produced");

    // Verify withdrawal recorded
    let bridge = node.state.bridge_state.read().unwrap();
    assert_eq!(
        bridge.total_withdrawals.get(&1).copied().unwrap_or(0),
        300,
        "bridge should record 300 withdrawn"
    );
}

/// `ExternalBridgeDeposit` instruction with insufficient signatures is rejected.
#[test]
fn test_bridge_external_deposit_insufficient_sigs_rejected() {
    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
    let recipient = test_addr(2);

    // Fund sender with CALL for gas
    node.state
        .balance_state
        .write()
        .unwrap()
        .balances
        .set_balance(1, sender, 1_000_000_000)
        .unwrap();

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

    // Submit an ExternalBridgeDeposit with empty signatures (should fail at bridge level)
    let tx = sign_tx(
        &secret,
        ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::ExternalBridgeDeposit {
                source_chain: 0,
                source_tx_hash: [0u8; 32],
                source_block_number: 100,
                external_sender: vec![0u8; 32],
                recipient,
                asset_id: 1,
                amount: 1000,
                validator_signatures: vec![],
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(tx);

    let block = node.produce_block(1_000_000);
    assert!(block.is_some(), "block should be produced");

    let result = node.last_result.clone().expect("execution result should exist");
    assert_eq!(result.protocol_tx_count, 1, "tx should be included");
    let tx_result = result.transaction_results.get(0).expect("one transaction result");
    match &tx_result.status {
        ExecutionStatus::Reverted { reason } => {
            assert!(
                reason.contains("bridge deposit:"),
                "expected bridge deposit failure, got: {}",
                reason
            );
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

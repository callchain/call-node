use crate::*;
use crate::ForkManager;
use call_primitives::{Address, BlockHash, Hash, ProtocolVersion};
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};
use call_bridge::BridgeOp;
use call_evm::{EvmExecutor, EvmTransaction};
use call_protocol::account::AccountState;
use call_protocol::registry::AssetRegistry;
use call_protocol::FeeParams;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn test_fork_manager() -> ForkManager {
    ForkManager::new(ProtocolVersion::new(1, 0, 0), 1)
}

const TEST_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0, 0);

fn make_test_evm_tx() -> EvmTransaction {
    EvmTransaction {
        caller: test_addr(1),
        nonce: 0,
        gas_limit: 21_000,
        gas_price: 1_000_000_000,
        to: Some(test_addr(2)),
        value: call_primitives::U256::from(100),
        data: call_evm::Bytes::default(),
        chain_id: 1,
    }
}

fn make_test_tx() -> ProtocolTransaction {
    ProtocolTransaction {
        sender: test_addr(1),
        nonce: 0,
        instructions: vec![Instruction::Transfer {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 100,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    }
}

/// Create a protocol transaction with a valid secp256k1 signature.
/// Returns the transaction and the sender address (derived from the signing key).
fn make_signed_test_tx() -> (ProtocolTransaction, Address) {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::Transfer {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 100,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };
    (tx, sender)
}

#[test]
fn test_block_structure_serialization() {
    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![make_test_tx()],
        vec![vec![0u8; 100]],
        vec![SystemTx {
            kind: SystemTxKind::UpdateBaseFee,
            data: vec![],
        }],
        vec![],
    );

    assert_eq!(block.header.height, 1);
    assert_eq!(block.header.proposer, 1);
    assert_eq!(block.protocol_txs.len(), 1);
    assert_eq!(block.evm_txs.len(), 1);
    assert_eq!(block.system_txs.len(), 1);
    assert_eq!(block.bridge_operations.len(), 0);
}

#[test]
fn test_block_header_hash() {
    let header = BlockHeader {
        parent_hash: BlockHash::ZERO,
        height: 1,
        timestamp_millis: 1000,
        payment_root: Hash::ZERO,
        evm_state_root: Hash::ZERO,
        bridge_root: Hash::ZERO,
        receipt_root: Hash::ZERO,
        state_root: Hash::ZERO,
        proposer: 1,
        signature: BlockSignature::default(),
        version: TEST_VERSION,
        bls_aggregate_signature: None,
        bls_signer_bitmap: Vec::new(),
    };

    let hash = header.hash();
    // Hash should be deterministic
    let hash2 = header.hash();
    assert_eq!(hash, hash2);
    // Hash should be 32 bytes
    assert_eq!(hash.as_slice().len(), 32);
}

#[test]
fn test_block_header_validate() {
    let header = BlockHeader {
        parent_hash: BlockHash::repeat_byte(1),
        height: 2,
        timestamp_millis: 1000,
        payment_root: Hash::ZERO,
        evm_state_root: Hash::ZERO,
        bridge_root: Hash::ZERO,
        receipt_root: Hash::ZERO,
        state_root: Hash::ZERO,
        proposer: 1,
        signature: BlockSignature::default(),
        version: TEST_VERSION,
        bls_aggregate_signature: None,
        bls_signer_bitmap: Vec::new(),
    };
    let fm = test_fork_manager();

    assert!(header.validate(BlockHash::repeat_byte(1), &fm).is_ok());
    assert!(header.validate(BlockHash::repeat_byte(2), &fm).is_err());
}

#[test]
fn test_block_validate() {
    let block = Block::new(
        1,
        BlockHash::repeat_byte(1),
        1000,
        1,
        TEST_VERSION,
        vec![make_test_tx()],
        vec![],
        vec![],
        vec![],
    );
    let fm = test_fork_manager();

    assert!(block.validate(BlockHash::repeat_byte(1), &fm).is_ok());
    assert!(block.validate(BlockHash::repeat_byte(2), &fm).is_err());
}

#[test]
fn test_block_validate_duplicate_nonce() {
    let tx = make_test_tx();
    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx.clone(), tx],
        vec![],
        vec![],
        vec![],
    );
    let fm = test_fork_manager();

    let result = block.validate(BlockHash::ZERO, &fm);
    assert!(result.is_err());
}

#[test]
fn test_block_execution_order() {
    let evm_tx = make_test_evm_tx();
    let evm_bytes = serde_json::to_vec(&evm_tx).unwrap();
    let (protocol_tx, sender) = make_signed_test_tx();
    let mut block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![protocol_tx],
        vec![evm_bytes],
        vec![SystemTx {
            kind: SystemTxKind::ValidatorReward {
                proposer: 1,
                reward: 1000,
            },
            data: vec![],
        }],
        vec![BridgeOp::DepositToEvm {
            asset_id: call_protocol::CALL_ASSET_ID,
            from: sender,
            to: test_addr(2),
            amount: 500,
        }],
    );

    // Setup balance state
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    let mut evm_state = call_evm::EvmState::new();
    evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000_000u128));
    evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

    // Deploy wrapped token contract for asset 1
    let deploy_executor = call_evm::EvmExecutor::new(1);
    let (contract_addr, deploy_result) = deploy_executor
        .deploy_erc20_template(
            sender,
            &mut evm_state,
            "CALL",
            "CALL",
            18,
            call_protocol::BRIDGE_EVM_ADDRESS,
            sender,
            call_primitives::U256::ZERO,
            call_primitives::U256::from(1u64),
        )
        .unwrap();
    assert!(deploy_result.success, "ERC-20 deploy failed");
    registry.set_evm_contract_address(1, contract_addr);

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut account, &mut registry, &mut compliance,
                &mut bridge_state, &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 1,
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: None,
            },
            &mut Subsystems::none(),
        )
        .unwrap();

    // Execution order: EVM(1) → Protocol(1) → Bridge(1) → System(1)
    assert_eq!(result.evm_tx_count, 1);
    assert_eq!(result.protocol_tx_count, 1);
    assert_eq!(result.bridge_op_count, 1);
    assert_eq!(result.system_tx_count, 1);

    // Finalize
    block.finalize(&result);
    assert_ne!(block.header.payment_root, Hash::ZERO);
    assert_ne!(block.header.bridge_root, Hash::ZERO);
}

#[test]
fn test_base_fee_update_after_block() {
    let mut fee_params = FeeParams::default();
    fee_params.base_fee = 100;

    // High gas usage should increase fee
    update_base_fee_after_block(&mut fee_params, 15_000_000);
    assert!(fee_params.base_fee > 100);

    // Low gas usage should decrease fee
    fee_params.base_fee = 100;
    update_base_fee_after_block(&mut fee_params, 5_000_000);
    assert!(fee_params.base_fee < 100);
}

#[test]
fn test_system_tx_reward_distribution() {
    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![],
        vec![],
        vec![SystemTx {
            kind: SystemTxKind::ValidatorReward {
                proposer: 1,
                reward: 500_000,
            },
            data: vec![],
        }],
        vec![],
    );

    let mut account = AccountState::new();
    let mut registry = AssetRegistry::new();
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut account, &mut registry, &mut compliance,
                &mut bridge_state, &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 1,
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: None,
            },
            &mut Subsystems::none(),
        )
        .unwrap();

    assert_eq!(result.total_validator_reward, 500_000);
}

#[test]
fn test_block_new_builder() {
    let block = Block::new(
        42,
        BlockHash::repeat_byte(0xFF),
        999_000,
        7,
        TEST_VERSION,
        vec![],
        vec![],
        vec![],
        vec![],
    );

    assert_eq!(block.header.height, 42);
    assert_eq!(block.header.parent_hash, BlockHash::repeat_byte(0xFF));
    assert_eq!(block.header.timestamp_millis, 999_000);
    assert_eq!(block.header.proposer, 7);
    assert_eq!(block.header.version, TEST_VERSION);
}

#[test]
fn test_block_execution_result() {
    let result = BlockExecutionResult {
        evm_tx_count: 10,
        protocol_tx_count: 5,
        bridge_op_count: 2,
        system_tx_count: 1,
        ..Default::default()
    };

    assert_eq!(result.total_tx_count(), 18);
}

#[test]
fn test_agent_instruction_emits_event() {
    use call_agent::{AgentBalances, AgentEventType, AgentRegistry};
    use call_protocol::instructions::AgentPayment;

    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    // Register agent
    let mut agent_registry = AgentRegistry::new_with_format_verifier();
    let agent_id = agent_registry
        .register_agent(
            sender,
            pubkey,
            "test-agent".into(),
            "https://test.com".into(),
            [0u8; 32],
            None,
            1,
            None,
        )
        .unwrap();

    // Fund owner and grant to agent
    let mut account = AccountState::new();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
    let mut agent_balances = AgentBalances::new();
    agent_balances
        .grant_funds(sender, agent_id, 1, 5_000, &mut account)
        .unwrap();

    // Build signed AgentPay transaction
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::AgentPay {
            payment: AgentPayment {
                agent_id,
                asset_id: call_protocol::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 1_000,
            },
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 100,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut account, &mut registry, &mut compliance,
                &mut bridge_state, &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 50, // current_block_height < expires_at
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: None,
            },
            &mut Subsystems {
                agent_balances: Some(&mut agent_balances),
                agent_registry: Some(&mut agent_registry),
                ..Subsystems::none()
            },
        )
        .unwrap();

    eprintln!("protocol_tx_count={} agent_events={}", result.protocol_tx_count, result.agent_events.len());
    // Verify agent event was emitted
    assert_eq!(result.agent_events.len(), 1);
    let event = &result.agent_events[0];
    assert!(matches!(event.event_type, AgentEventType::AgentPay));
    assert_eq!(event.agent_id, agent_id);
    assert_eq!(event.asset_id, 1);
    assert_eq!(event.amount, 1_000);
    assert_eq!(event.recipient, Some(test_addr(2)));
    assert_eq!(event.block_height, 50);

    // Verify receipt root includes agent events
    let receipt_root = compute_receipt_root(&result);
    assert_ne!(receipt_root, Hash::ZERO);
}

#[test]
fn test_expired_transaction_rejected() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::Transfer {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 100,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 50, // expires at block 50
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
    let mut registry = AssetRegistry::new();
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state,
                        &mut evm_state,
                    ),
        &mut BlockContext {
                        current_block_height: 51,
                        fee_params: &mut fee_params,
                        bridge_config: Some(&bridge_config),
                        validators: None,
                    },
        &mut Subsystems::none(),
    );

    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("expired"), "expected expiry error, got: {err_msg}");
}

#[test]
fn test_frozen_asset_rejects_bridge_to_evm() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::BridgeToEvm {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 500,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    // Freeze CALL asset
    registry.freeze_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let bridge_config = call_bridge::BridgeConfig::default();

    let validators: Vec<Address> = vec![sender];
    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext {
                        current_block_height: 1,
                        fee_params: &mut fee_params,
                        bridge_config: Some(&bridge_config),
                        validators: Some(&validators),
                    },
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("not active"), "expected not-active error, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

#[test]
fn test_delisted_asset_rejects_bridge_to_protocol() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::BridgeToProtocol {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 500,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    // Delist CALL asset
    registry.delist_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000u128));
    let bridge_config = call_bridge::BridgeConfig::default();

    let validators: Vec<Address> = vec![sender];
    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext {
                        current_block_height: 1,
                        fee_params: &mut fee_params,
                        bridge_config: Some(&bridge_config),
                        validators: Some(&validators),
                    },
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("not active"), "expected not-active error, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

#[test]
fn test_frozen_asset_rejects_bridge_op_deposit() {
    let sender = test_addr(1);
    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![],
        vec![],
        vec![],
        vec![BridgeOp::DepositToEvm {
            asset_id: call_protocol::CALL_ASSET_ID,
            from: sender,
            to: test_addr(2),
            amount: 500,
        }],
    );

    let mut account = AccountState::new();
    account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry.freeze_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext {
                        current_block_height: 1,
                        fee_params: &mut fee_params,
                        bridge_config: Some(&bridge_config),
                        validators: None,
                    },
        &mut Subsystems::none(),
    );

    assert!(result.is_err(), "BridgeOp DepositToEvm on frozen asset should fail");
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("not active"), "expected not-active error, got: {err_msg}");
}

#[test]
fn test_evm_issuer_mint_success() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);
    let recipient = test_addr(2);

    // Deploy ERC-20 contract first
    let executor = EvmExecutor::new(1);
    let mut evm_state = call_evm::EvmState::new();
    let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
    evm_state.create_account(deployer);
    evm_state.create_account(sender);
    evm_state.set_balance(sender, call_primitives::U256::from(10_000_000u128));

    let (contract_addr, deploy_result) = executor
        .deploy_erc20_template(
            deployer,
            &mut evm_state,
            "Test Token",
            "TST",
            18,
            deployer,
            sender,
            call_primitives::U256::from(10_000),
            call_primitives::U256::from(2u64),
        )
        .unwrap();
    assert!(deploy_result.success, "ERC-20 deploy failed");

    // Register asset with max_supply cap
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry
        .register_asset("TST".into(), "Test Token".into(), 18, sender, 0, 0, 10_000)
        .unwrap();
    registry.set_evm_contract_address(2, contract_addr);

    // Build EvmIssuerMint instruction
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::EvmIssuerMint {
            asset_id: 2,
            to: recipient,
            amount: 5_000,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 200_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    assert!(result.is_ok(), "EvmIssuerMint should succeed: {:?}", result);

    // evm_supply should be updated
    let asset = registry.get_asset(2).unwrap();
    assert_eq!(asset.evm_supply, 5_000);
    assert_eq!(asset.protocol_supply, 0);
    assert_eq!(asset.all_supply(), 5_000);

    // EVM state reflects the mint (contract storage updated, verified by success + supply tracking)
}

#[test]
fn test_evm_issuer_mint_cap_enforcement() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    // Deploy ERC-20 with small cap
    let executor = EvmExecutor::new(1);
    let mut evm_state = call_evm::EvmState::new();
    let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
    evm_state.create_account(deployer);

    let (contract_addr, deploy_result) = executor
        .deploy_erc20_template(
            deployer,
            &mut evm_state,
            "Capped",
            "CAP",
            18,
            deployer,
            sender,
            call_primitives::U256::from(1_000),
            call_primitives::U256::from(2u64),
        )
        .unwrap();
    assert!(deploy_result.success);

    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry
        .register_asset("CAP".into(), "Capped".into(), 18, sender, 0, 0, 1_000)
        .unwrap();
    registry.set_evm_contract_address(2, contract_addr);

    // Try to mint more than cap
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::EvmIssuerMint {
            asset_id: 2,
            to: test_addr(2),
            amount: 1_001,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 200_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("cap exceeded"), "expected cap error, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

#[test]
fn test_evm_issuer_mint_non_issuer_rejected() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);
    let issuer = test_addr(1); // different from sender

    let executor = EvmExecutor::new(1);
    let mut evm_state = call_evm::EvmState::new();
    let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
    evm_state.create_account(deployer);

    let (contract_addr, deploy_result) = executor
        .deploy_erc20_template(
            deployer,
            &mut evm_state,
            "Test",
            "TST",
            18,
            deployer,
            issuer,
            call_primitives::U256::from(0),
            call_primitives::U256::from(2u64),
        )
        .unwrap();
    assert!(deploy_result.success);

    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry
        .register_asset("TST".into(), "Test".into(), 18, issuer, 0, 0, 0)
        .unwrap();
    registry.set_evm_contract_address(2, contract_addr);

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::EvmIssuerMint {
            asset_id: 2,
            to: test_addr(2),
            amount: 500,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 200_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("not asset issuer"), "expected unauthorized error, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

#[test]
fn test_evm_issuer_mint_frozen_asset_rejected() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let executor = EvmExecutor::new(1);
    let mut evm_state = call_evm::EvmState::new();
    let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
    evm_state.create_account(deployer);

    let (contract_addr, deploy_result) = executor
        .deploy_erc20_template(
            deployer,
            &mut evm_state,
            "Test",
            "TST",
            18,
            deployer,
            sender,
            call_primitives::U256::from(0),
            call_primitives::U256::from(2u64),
        )
        .unwrap();
    assert!(deploy_result.success);

    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry
        .register_asset("TST".into(), "Test".into(), 18, sender, 0, 0, 0)
        .unwrap();
    registry.set_evm_contract_address(2, contract_addr);
    registry.freeze_asset(2, &sender).unwrap();

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::EvmIssuerMint {
            asset_id: 2,
            to: test_addr(2),
            amount: 500,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 200_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("not active"), "expected not-active error, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

#[test]
fn test_evm_issuer_mint_call_asset_rejected() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::EvmIssuerMint {
            asset_id: call_protocol::CALL_ASSET_ID,
            to: test_addr(2),
            amount: 500,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 200_000,
        max_fee: 1_000_000,
        expires_at: 0,
        auth: AuthScheme::SingleSig {
            signature: [0u8; 65],
        },
    };
    let tx_hash = tx.compute_tx_hash();
    let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
    tx.auth = AuthScheme::SingleSig { signature };

    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![tx],
        vec![],
        vec![],
        vec![],
    );

    let mut account = AccountState::new();
    account
        .balances
        .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.instruction_results.is_empty(), "expected at least one instruction result");
    match &result.instruction_results[0] {
        InstructionResult::Reverted { reason } => {
            assert!(reason.contains("CALL asset has no EVM wrapped token"), "expected CALL rejection, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

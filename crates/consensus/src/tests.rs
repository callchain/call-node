use crate::*;
use crate::ForkManager;
use crate::exec::evm_instructions;
use call_primitives::{Address, BlockHash, Hash, ProtocolVersion, ExecutionStatus};
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};
use call_bridge::BridgeOp;
use call_evm::{EvmExecutor, EvmTransaction};
use call_protocol::account::AccountState;
use call_protocol::registry::AssetRegistry;
use call_protocol::FeeParams;
use call_precompiles::{address_to_u256, BRIDGE_ADDRESS};
use crate::exec::evm_instructions::slot_bridge_contract;

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
        max_priority_fee: 1,
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
        max_priority_fee: 1,
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
    evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000_000u128));
    evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

    // Deploy wrapped token contract for asset 1
    let deployer = test_addr(99);
    evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000_000u128));
    evm_state.create_account(deployer);
    let deploy_executor = call_evm::EvmExecutor::new(1);
    let (contract_addr, deploy_result) = deploy_executor
        .deploy_erc20_template(
            deployer,
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

    // Seed EVM storage for bridge op (asset metadata + contract address)
    evm_instructions::seed_asset(
        &mut evm_state,
        1,
        "CALL",
        "Callchain",
        18,
        sender,
        0,
        0,
        0, // active
    );
    evm_state.set_storage(
        BRIDGE_ADDRESS,
        evm_instructions::slot_bridge_contract(1),
        address_to_u256(contract_addr),
    );

    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut account, &mut registry, &mut compliance,
                &mut shielded_state, &mut evm_state,
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
    assert_ne!(block.header.state_root, Hash::ZERO);
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
                &mut shielded_state, &mut evm_state,
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
fn test_agent_pay_via_evm_storage() {
    use call_protocol::instructions::AgentPayment;
    use call_precompiles::{address_to_u256, u128_to_u256, u64_to_u256, AGENT_ADDRESS};
    use crate::exec::evm_instructions::{
        agent_get_balance, agent_set_balance, pack_agent_perms, read_balance,
        seed_balance, slot_agent_count, slot_agent_name, slot_agent_owner,
        slot_agent_perms, slot_agent_pubkey, slot_agent_registered_at, slot_agent_url,
    };

    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);
    let agent_id: u64 = 0;
    let recipient = test_addr(2);

    let mut account = AccountState::new();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
        .unwrap();
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();

    // Seed sender balance for fee + nothing else needed (agent pays from agent balance)
    seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000);

    // Seed agent registration directly in EVM storage
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(1));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_owner(agent_id), address_to_u256(sender));
    let pk_hash = call_crypto::keccak256(&pubkey);
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_pubkey(agent_id), alloy_primitives::U256::from_be_slice(pk_hash.as_slice()));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_name(agent_id), call_precompiles::write_string32("test-agent"));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_url(agent_id), call_precompiles::write_string32("https://test.com"));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_perms(agent_id), pack_agent_perms(10_000, 0, 1));
    evm_state.set_storage(AGENT_ADDRESS, slot_agent_registered_at(agent_id), u64_to_u256(1));

    // Seed agent balance
    agent_set_balance(&mut evm_state, agent_id, call_protocol::CALL_ASSET_ID, 5_000);

    // Build signed AgentPay transaction
    let mut tx = ProtocolTransaction {
        sender,
        nonce: 0,
        instructions: vec![Instruction::AgentPay {
            payment: AgentPayment {
                agent_id,
                asset_id: call_protocol::CALL_ASSET_ID,
                to: recipient,
                amount: 1_000,
            },
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        max_priority_fee: 1,
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

    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut account, &mut registry, &mut compliance,
                &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 50,
                fee_params: &mut fee_params,
                bridge_config: Some(&bridge_config),
                validators: None,
            },
            &mut Subsystems::none(),
        )
        .unwrap();

    assert_eq!(result.protocol_tx_count, 1);
    assert_eq!(result.transaction_results[0].status, ExecutionStatus::Success);

    // Verify agent balance decreased and recipient received funds
    assert_eq!(agent_get_balance(&evm_state, agent_id, call_protocol::CALL_ASSET_ID), 4_000);
    assert_eq!(read_balance(&evm_state, call_protocol::CALL_ASSET_ID, recipient), 1_000);
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
        max_priority_fee: 1,
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
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
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
        max_priority_fee: 1,
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
    evm_instructions::seed_asset(&mut evm_state, call_protocol::CALL_ASSET_ID, "CALL", "Callchain", 18, sender, 0, 0, 1);
    let bridge_config = call_bridge::BridgeConfig::default();

    let validators: Vec<Address> = vec![sender];
    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
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
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
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
        max_priority_fee: 1,
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
    evm_instructions::seed_asset(&mut evm_state, call_protocol::CALL_ASSET_ID, "CALL", "Callchain", 18, sender, 0, 0, 2);
    evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000u128));
    let bridge_config = call_bridge::BridgeConfig::default();

    let validators: Vec<Address> = vec![sender];
    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
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
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
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
    // Seed EVM storage with frozen asset status
    evm_instructions::seed_asset(
        &mut evm_state,
        call_protocol::CALL_ASSET_ID,
        "CALL",
        "Callchain",
        18,
        sender,
        0,
        0,
        1, // frozen
    );
    let bridge_config = call_bridge::BridgeConfig::default();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
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

    // Seed asset metadata and contract address in EVM storage
    evm_instructions::seed_asset(
        &mut evm_state, 2, "TST", "Test Token", 18, sender, 10_000, 0, 0,
    );
    use call_precompiles::{address_to_u256, BRIDGE_ADDRESS};
    use crate::exec::evm_instructions::slot_bridge_contract;
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(2), address_to_u256(contract_addr));

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
        max_priority_fee: 1,
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

    let mut registry = AssetRegistry::new();
    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    assert!(result.is_ok(), "EvmIssuerMint should succeed: {:?}", result);

    // Verify supply updated in EVM storage
    let supply = call_precompiles::u256_to_u128(
        evm_state.get_storage(
            &call_precompiles::ASSET_ADDRESS,
            call_precompiles::slot_asset_meta(2, b"supply"),
        ));
    assert_eq!(supply, 5_000);
}

#[test]
fn test_evm_issuer_mint_cap_enforcement() {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let sender = call_crypto::pubkey_to_address(&pubkey);

    // Deploy ERC-20 with small cap
    let executor = EvmExecutor::new(1);
    let mut evm_state = call_evm::EvmState::new();
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
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

    // Seed asset metadata and contract address in EVM storage
    evm_instructions::seed_asset(
        &mut evm_state, 2, "CAP", "Capped", 18, sender, 1_000, 0, 0,
    );
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(2), address_to_u256(contract_addr));

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
        max_priority_fee: 1,
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
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut registry = AssetRegistry::new();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
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

    // Seed asset metadata with different issuer
    evm_instructions::seed_asset(
        &mut evm_state, 2, "TST", "Test", 18, issuer, 0, 0, 0,
    );
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(2), address_to_u256(contract_addr));

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
        max_priority_fee: 1,
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
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut registry = AssetRegistry::new();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
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
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
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

    // Seed asset with frozen status (1 = Frozen)
    evm_instructions::seed_asset(
        &mut evm_state, 2, "TST", "Test", 18, sender, 0, 0, 1,
    );
    evm_state.set_storage(BRIDGE_ADDRESS, slot_bridge_contract(2), address_to_u256(contract_addr));

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
        max_priority_fee: 1,
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
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut registry = AssetRegistry::new();

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
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
        max_priority_fee: 1,
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
    let mut compliance = call_protocol::compliance::ComplianceEngine::new();
    let mut bridge_state = call_bridge::BridgeStateManager::default();
    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();
    let mut registry = AssetRegistry::new();
    evm_instructions::seed_balance(&mut evm_state, call_protocol::CALL_ASSET_ID, sender, 1_000_000_000);
    // Seed CALL asset in EVM storage so issuer check succeeds
    evm_instructions::seed_asset(
        &mut evm_state, call_protocol::CALL_ASSET_ID, "CALL", "Callchain", 18, sender, 0, 0, 0,
    );

    let result = block.execute(
        &mut ExecutionState::new(
                        &mut account, &mut registry, &mut compliance, &mut shielded_state, &mut evm_state,
                    ),
        &mut BlockContext::new(1, &mut fee_params),
        &mut Subsystems::none(),
    );

    let result = result.expect("block execute should not fail");
    assert!(!result.transaction_results.is_empty(), "expected at least one transaction result");
    match &result.transaction_results[0].status {
        ExecutionStatus::Reverted { reason } => {
            assert!(reason.contains("CALL asset has no EVM wrapped token"), "expected CALL rejection, got: {reason}");
        }
        other => panic!("expected Reverted, got {:?}", other),
    }
}

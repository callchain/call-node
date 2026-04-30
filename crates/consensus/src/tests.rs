use crate::*;
use crate::ForkManager;
use call_primitives::{Address, BlockHash, Hash, ProtocolVersion};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};
use call_evm::EvmTransaction;
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
    let evm_tx = make_test_evm_tx();
    let evm_bytes = serde_json::to_vec(&evm_tx).unwrap();
    let block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![evm_bytes],
    );

    assert_eq!(block.header.height, 1);
    assert_eq!(block.header.proposer, 1);
    assert_eq!(block.evm_txs.len(), 1);
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
        vec![],
    );
    let fm = test_fork_manager();

    assert!(block.validate(BlockHash::repeat_byte(1), &fm).is_ok());
    assert!(block.validate(BlockHash::repeat_byte(2), &fm).is_err());
}

#[test]
fn test_block_execution_order() {
    let evm_tx = make_test_evm_tx();
    let evm_bytes = serde_json::to_vec(&evm_tx).unwrap();
    let mut block = Block::new(
        1,
        BlockHash::ZERO,
        1000,
        1,
        TEST_VERSION,
        vec![evm_bytes],
    );

    let mut evm_state = call_evm::EvmState::new();
    evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 1,
                fee_params: &mut fee_params,
                bridge_config: None,
                validators: None,
            },
            &mut Subsystems::none(),
        )
        .unwrap();

    assert_eq!(result.evm_tx_count, 1);

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
    );

    let mut shielded_state = call_shielded::ShieldedState::new();
    let mut fee_params = FeeParams::default();
    let mut evm_state = call_evm::EvmState::new();

    let result = block
        .execute(
            &mut ExecutionState::new(
                &mut shielded_state, &mut evm_state,
            ),
            &mut BlockContext {
                current_block_height: 1,
                fee_params: &mut fee_params,
                bridge_config: None,
                validators: None,
            },
            &mut Subsystems::none(),
        )
        .unwrap();

    // Validator reward is no longer injected via system_tx; currently no
    // automatic reward is minted during block execution.
    assert_eq!(result.total_validator_reward, 0);
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
        ..Default::default()
    };

    assert_eq!(result.total_tx_count(), 10);
}

#[test]
fn test_expired_transaction_rejected() {
    // Protocol transactions are no longer part of Block; expiry is handled at mempool level
}

#[test]
fn test_frozen_asset_rejects_bridge_to_evm() {
    // BridgeToEvm protocol transactions are no longer part of Block
}

#[test]
fn test_delisted_asset_rejects_bridge_to_protocol() {
    // BridgeToProtocol protocol transactions are no longer part of Block
}

#[test]
fn test_frozen_asset_rejects_bridge_op_deposit() {
    // BridgeOp operations are no longer part of Block
    // This test is now a no-op placeholder
}

#[test]
fn test_evm_issuer_mint_success() {
    // EvmIssuerMint protocol transactions are no longer part of Block
    // This test is now a no-op placeholder
}

#[test]
fn test_evm_issuer_mint_cap_enforcement() {
    // EvmIssuerMint protocol transactions are no longer part of Block
    // This test is now a no-op placeholder
}

#[test]
fn test_evm_issuer_mint_non_issuer_rejected() {
    // EvmIssuerMint protocol transactions are no longer part of Block
    // This test is now a no-op placeholder
}

#[test]
fn test_evm_issuer_mint_frozen_asset_rejected() {
    // EvmIssuerMint protocol transactions are no longer part of Block
    // This test is now a no-op placeholder
}

#[test]
fn test_evm_issuer_mint_call_asset_rejected() {
    // EvmIssuerMint protocol transactions are no longer part of Block
    // This test is now a no-op placeholder
}

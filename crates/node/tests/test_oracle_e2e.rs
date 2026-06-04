//! E2E test: Oracle price submission and retrieval via TestNode harness
//!
//! Validates that the oracle precompile (0x101) correctly stores and
//! aggregates price data when called through EVM transactions.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use call_oracle::precompile::{IProtocolOracle, ORACLE_ADDRESS};
use call_precompile::storage::storage_slot;

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Compute an oracle price storage slot.
fn oracle_price_slot(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"price"])
}

/// Compute an oracle TWAP storage slot.
fn oracle_twap_slot(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"twap"])
}

/// Compute an oracle count storage slot.
fn oracle_count_slot(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"count"])
}

/// Oracle price submission through EVM precompile, verified by direct storage read.
#[test]
fn test_oracle_price_submit_and_read() {
    let mut node = TestNode::new();

    let (_validator_secret, validator_addr) = test_keypair();

    // Stake a validator so there is a proposer and an authorized oracle submitter
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let _ = consensus
            .stake_validator(
                provider.state_mut(),
                validator_addr,
                [1u8; 32],
                one_million_call(),
            )
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Seed native EVM balance for gas payment and protocol balance for fees
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            validator_addr,
            one_million_call(),
        );
        provider.state_mut().set_balance(
            validator_addr,
            call_primitives::U256::from(100_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Track asset 1 so oracle submissions are accepted
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_storage(
            ORACLE_ADDRESS,
            storage_slot(&[b"tracked_count"]),
            alloy_primitives::U256::from(1u64),
        );
        provider.state_mut().set_storage(
            ORACLE_ADDRESS,
            storage_slot(&[b"tracked", &0u64.to_be_bytes()[..]]),
            alloy_primitives::U256::from(1u64),
        );
        provider.state_mut().set_storage(
            ORACLE_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"flag"]),
            alloy_primitives::U256::from(1u64),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Verify validator is registered before submission
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let validator_id = call_consensus::exec::state_accessors::read_validator_id_by_addr(
            provider.state(),
            validator_addr,
        );
        assert_ne!(validator_id, 0, "validator should be registered");
    }

    let asset_id = 1u64;
    let price1 = 50_000_000_000_000_000u128; // within MAX_PRICE (10^18)

    // Step 1: Submit first price
    let submit_data = IProtocolOracle::submitPriceCall {
        assetId: asset_id,
        price: price1,
        timestamp: 1_000_000,
        blockNumber: 0,
    }
    .abi_encode();

    let submit_tx = call_evm::EvmTransaction {
        caller: validator_addr,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(ORACLE_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(submit_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(submit_tx);
    let result = node.produce_block(1_000_000);
    assert!(result.is_some(), "block production failed");
    let last_result = node.last_result.as_ref().unwrap();
    assert_eq!(last_result.evm_tx_results.len(), 1);
    assert!(
        last_result.evm_tx_results[0].status,
        "oracle submit should succeed"
    );

    // Verify price, count, and TWAP after first submission
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();

        let stored_price = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_price_slot(asset_id))
            .to_be_bytes::<32>();
        let price_u128 = u128::from_be_bytes(stored_price[16..32].try_into().unwrap());
        assert_eq!(price_u128, price1, "price should match first submission");

        let stored_count = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_count_slot(asset_id))
            .to_be_bytes::<32>();
        let count_u64 = u64::from_be_bytes(stored_count[24..32].try_into().unwrap());
        assert_eq!(count_u64, 1, "count should be 1 after first submission");

        let stored_twap = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_twap_slot(asset_id))
            .to_be_bytes::<32>();
        let twap_u128 = u128::from_be_bytes(stored_twap[16..32].try_into().unwrap());
        assert_eq!(
            twap_u128, price1,
            "twap should equal first price when count=1"
        );
    }

    // Step 2: Submit second price to test TWAP averaging
    let price2 = 60_000_000_000_000_000u128; // within MAX_PRICE
    let submit_data2 = IProtocolOracle::submitPriceCall {
        assetId: asset_id,
        price: price2,
        timestamp: 1_000_250,
        blockNumber: 1,
    }
    .abi_encode();

    let submit_tx2 = call_evm::EvmTransaction {
        caller: validator_addr,
        nonce: 1,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(ORACLE_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(submit_data2),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(submit_tx2);
    node.produce_block(1_000_250);

    // Verify TWAP = (50k + 60k) / 2 = 55k
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();

        let stored_price = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_price_slot(asset_id))
            .to_be_bytes::<32>();
        let price_u128 = u128::from_be_bytes(stored_price[16..32].try_into().unwrap());
        assert_eq!(price_u128, price2, "price should match second submission");

        let stored_count = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_count_slot(asset_id))
            .to_be_bytes::<32>();
        let count_u64 = u64::from_be_bytes(stored_count[24..32].try_into().unwrap());
        assert_eq!(count_u64, 2, "count should be 2 after second submission");

        let expected_twap = (price1 + price2) / 2;
        let stored_twap = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_twap_slot(asset_id))
            .to_be_bytes::<32>();
        let twap_u128 = u128::from_be_bytes(stored_twap[16..32].try_into().unwrap());
        assert_eq!(
            twap_u128, expected_twap,
            "twap should be average of two prices"
        );
    }
}

/// Non-validator oracle submission should be rejected.
#[test]
fn test_oracle_non_validator_rejected() {
    let mut node = TestNode::new();

    let (_validator_secret, validator_addr) = test_keypair();
    let (_non_validator_secret, non_validator_addr) = test_keypair();

    // Stake validator (needed for proposer) but submit from non-validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let _ = consensus
            .stake_validator(
                provider.state_mut(),
                validator_addr,
                [1u8; 32],
                one_million_call(),
            )
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Seed non-validator with gas balance
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(
            non_validator_addr,
            call_primitives::U256::from(100_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Submit from non-validator
    let submit_data = IProtocolOracle::submitPriceCall {
        assetId: 1,
        price: 1_000_000,
        timestamp: 1_000_000,
        blockNumber: 0,
    }
    .abi_encode();

    let submit_tx = call_evm::EvmTransaction {
        caller: non_validator_addr,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(ORACLE_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(submit_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(submit_tx);
    let result = node.produce_block(1_000_000);
    assert!(result.is_some());

    // Price should remain zero (default) because submission was rejected
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let stored_price = provider
            .state()
            .get_storage(&ORACLE_ADDRESS, oracle_price_slot(1))
            .to_be_bytes::<32>();
        let price_u128 = u128::from_be_bytes(stored_price[16..32].try_into().unwrap());
        assert_eq!(price_u128, 0, "non-validator submission should be rejected");
    }
}

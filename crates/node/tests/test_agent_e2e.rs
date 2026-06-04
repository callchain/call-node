//! E2E test: Agent registration, balance grant, pay, and revoke via TestNode harness
//!
//! Validates that the agent precompile (0x209) correctly processes agent
//! lifecycle transactions when included in blocks.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use alloy_sol_types::SolCall;
use call_agent::precompile::IProtocolAgent;
use call_agent::{slot_agent_balance, slot_agent_owner};
use call_precompile::AGENT_ADDRESS;

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Full agent lifecycle: register -> grant -> pay -> batchPay -> revokeBalance -> revokeAgent.
#[test]
fn test_agent_lifecycle_in_block() {
    let mut node = TestNode::new();

    let (_owner_secret, owner) = test_keypair();
    let (_validator_secret, validator_addr) = test_keypair();
    let recipient = test_addr(0x33);

    // Stake a validator so there is a proposer
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

    // Seed owner with protocol CALL balance and EVM gas balance
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            owner,
            one_million_call() * 5,
        );
        provider
            .state_mut()
            .set_balance(owner, call_primitives::U256::from(100_000_000_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Step 1: Register agent
    let register_data = IProtocolAgent::registerAgentCall {
        name: "TestAgent".into(),
        url: "https://agent.example.com".into(),
        agentAddress: owner,
    }
    .abi_encode();

    let register_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(register_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(register_tx);
    let result = node.produce_block(1_000_000);
    assert!(result.is_some(), "block production failed");

    // Verify agent owner via storage
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let owner_slot = slot_agent_owner(0);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, owner_slot);
        let agent_owner = call_primitives::Address::from_slice(&stored.to_be_bytes::<32>()[12..32]);
        assert_eq!(agent_owner, owner, "agent owner should match");
    }

    // Step 2: Grant balance to agent
    // Default per_tx_limit is 1_000; grant enough for multiple small pays
    let grant_amount = 5_000u128;
    let grant_data = IProtocolAgent::grantBalanceCall {
        agentId: 0,
        assetId: call_protocol::CALL_ASSET_ID,
        amount: grant_amount,
    }
    .abi_encode();

    let grant_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 1,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(grant_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(grant_tx);
    node.produce_block(1_000_001);

    // Verify agent balance
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let bal_slot = slot_agent_balance(0, call_protocol::CALL_ASSET_ID);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, bal_slot);
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&stored.to_be_bytes::<32>()[16..32]);
            buf
        });
        assert_eq!(bal, grant_amount, "agent balance should match grant");
    }

    // Step 3: Agent pays recipient (within default per_tx_limit of 1_000)
    let pay_amount = 1_000u128;
    let pay_data = IProtocolAgent::payCall {
        assetId: call_protocol::CALL_ASSET_ID,
        to: recipient,
        amount: pay_amount,
    }
    .abi_encode();

    let pay_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 2,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(pay_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(pay_tx);
    node.produce_block(1_000_002);

    // Verify agent balance decreased and recipient received tokens
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let bal_slot = slot_agent_balance(0, call_protocol::CALL_ASSET_ID);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, bal_slot);
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&stored.to_be_bytes::<32>()[16..32]);
            buf
        });
        assert_eq!(
            bal,
            grant_amount - pay_amount,
            "agent balance should decrease after pay"
        );

        let recipient_bal = call_consensus::exec::state_accessors::read_balance(
            provider.state(),
            call_protocol::CALL_ASSET_ID,
            recipient,
        );
        assert_eq!(
            recipient_bal, pay_amount,
            "recipient should receive payment"
        );
    }

    // Step 4: BatchPay to multiple recipients
    let r1 = test_addr(0x44);
    let r2 = test_addr(0x55);
    let batch_data = IProtocolAgent::batchPayCall {
        assetId: call_protocol::CALL_ASSET_ID,
        to: vec![r1, r2],
        amounts: vec![500, 500],
    }
    .abi_encode();

    let batch_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 3,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(batch_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(batch_tx);
    node.produce_block(1_000_003);

    // Verify batchPay results
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let bal_slot = slot_agent_balance(0, call_protocol::CALL_ASSET_ID);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, bal_slot);
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&stored.to_be_bytes::<32>()[16..32]);
            buf
        });
        assert_eq!(
            bal,
            grant_amount - pay_amount - 1_000,
            "agent balance should decrease after batchPay"
        );

        let r1_bal = call_consensus::exec::state_accessors::read_balance(
            provider.state(),
            call_protocol::CALL_ASSET_ID,
            r1,
        );
        let r2_bal = call_consensus::exec::state_accessors::read_balance(
            provider.state(),
            call_protocol::CALL_ASSET_ID,
            r2,
        );
        assert_eq!(r1_bal, 500);
        assert_eq!(r2_bal, 500);
    }

    // Step 5: Revoke balance
    let revoke_data = IProtocolAgent::revokeBalanceCall {
        agentId: 0,
        assetId: call_protocol::CALL_ASSET_ID,
    }
    .abi_encode();

    let revoke_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 4,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(revoke_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(revoke_tx);
    node.produce_block(1_000_004);

    // Verify agent balance is zero after revoke
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let bal_slot = slot_agent_balance(0, call_protocol::CALL_ASSET_ID);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, bal_slot);
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&stored.to_be_bytes::<32>()[16..32]);
            buf
        });
        assert_eq!(bal, 0, "agent balance should be zero after revoke");
    }

    // Step 6: Revoke agent
    let revoke_agent_data = IProtocolAgent::revokeAgentCall { agentId: 0 }.abi_encode();

    let revoke_agent_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 5,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(revoke_agent_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(revoke_agent_tx);
    node.produce_block(1_000_005);

    // Verify agent is revoked (owner should be zero address)
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let owner_slot = slot_agent_owner(0);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, owner_slot);
        let agent_owner = call_primitives::Address::from_slice(&stored.to_be_bytes::<32>()[12..32]);
        assert_eq!(
            agent_owner,
            call_primitives::Address::ZERO,
            "revoked agent owner should be zero"
        );
    }
}

/// Non-owner attempting to operate on an agent should be rejected.
#[test]
fn test_agent_non_owner_rejected() {
    let mut node = TestNode::new();

    let (_owner_secret, owner) = test_keypair();
    let (_non_owner_secret, non_owner) = test_keypair();
    let (_validator_secret, validator_addr) = test_keypair();

    // Stake validator
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

    // Seed both users
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            owner,
            one_million_call(),
        );
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            non_owner,
            one_million_call(),
        );
        provider
            .state_mut()
            .set_balance(owner, call_primitives::U256::from(100_000_000_000u128));
        provider
            .state_mut()
            .set_balance(non_owner, call_primitives::U256::from(100_000_000_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Owner registers agent
    let register_data = IProtocolAgent::registerAgentCall {
        name: "Agent".into(),
        url: "url".into(),
        agentAddress: owner,
    }
    .abi_encode();
    let register_tx = call_evm::EvmTransaction {
        caller: owner,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(register_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(register_tx);
    node.produce_block(1_000_000);

    // Non-owner tries to grant balance — should fail (revert)
    let grant_data = IProtocolAgent::grantBalanceCall {
        agentId: 0,
        assetId: call_protocol::CALL_ASSET_ID,
        amount: 100_000,
    }
    .abi_encode();
    let grant_tx = call_evm::EvmTransaction {
        caller: non_owner,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(AGENT_ADDRESS).into_array()[..20],
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(grant_data),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    };
    node.insert_evm_tx(grant_tx);
    node.produce_block(1_000_001);

    // Verify agent balance is still zero (grant was rejected)
    {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let bal_slot = slot_agent_balance(0, call_protocol::CALL_ASSET_ID);
        let stored = provider.state().get_storage(&AGENT_ADDRESS, bal_slot);
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&stored.to_be_bytes::<32>()[16..32]);
            buf
        });
        assert_eq!(bal, 0, "non-owner grant should be rejected");
    }
}

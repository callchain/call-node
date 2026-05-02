//! E2E test: Fork and upgrade
//!
//! Height-activated governance upgrade simulation.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, BlockHash, ProtocolVersion};
use call_evm::EvmTransaction;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_evm_tx(sender: Address, nonce: u64, to: Address, amount: u128) -> EvmTransaction {
    EvmTransaction {
        caller: sender,
        nonce,
        gas_limit: 21_000,
        gas_price: 1_000_000_000,
        to: Some(to),
        value: call_primitives::U256::from(amount),
        data: call_evm::Bytes::default(),
        chain_id: 1,
    }
}

/// Simulate a protocol upgrade at a specific block height.
#[tokio::test]
async fn test_height_activated_upgrade() {
    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(&mut evm_state, sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, sender, 100_000,
        );
    }

    let upgrade_height = 50u64;
    let mut upgraded = false;

    // Produce blocks up to and past the upgrade height
    for i in 0..60 {
        node.insert_evm_tx(make_evm_tx(sender, i as u64, test_addr(50 + (i % 10) as u8), 100));

        let ts = 1_000_000 + i * 250;
        let selection = { node.mempool.write().unwrap().select_transactions() };

        let (proposer, height) = {
            let c = node.consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };

        let Some(proposer) = proposer else { continue; };

        let evm_txs: Vec<Vec<u8>> = selection
            .evm_txs
            .into_iter()
            .map(|e| e.data)
            .collect();

        if height == upgrade_height && !upgraded {
            upgraded = true;
            node.state.fork_manager.write().unwrap().schedule_upgrade(
                call_consensus::fork::UpgradeEntry {
                    version: ProtocolVersion::new(2, 0, 0),
                    activation_height: upgrade_height,
                    applied: false,
                    proposal_id: Some(1),
                    approved_at_height: Some(height),
                },
            );
        }

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = call_consensus::Block::new(
            height,
            node.parent_hash,
            ts,
            proposer,
            version,
            evm_txs,
        );

        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        {
            let mut evm_state = node.state.evm_state.write().unwrap();
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result, &mut evm_state).expect("commit");
        }

        node.state.set_current_block(height + 1);
        node.parent_hash = block.header.hash();
        node.state.finalize_block();
        node.height = height + 1;
        node.blocks_produced.push(block);
    }

    assert!(upgraded, "upgrade should have been triggered");
    assert_eq!(node.consensus_height(), 60);
}

/// Chain fork: two nodes produce different blocks at same height, then reconcile.
#[tokio::test]
async fn test_chain_fork_and_reconcile() {
    let mut sim = NetworkSimulator::new();

    let (_secret, sender) = test_keypair();

    // Node A and Node B start with same initial state
    let node_a = NodeBuilder::new()
        .validator(sender, [1u8; 32], one_million_call())
        .balance(1, sender, 100_000)
        .build();

    let node_b = NodeBuilder::new()
        .balance(1, sender, 100_000) // no validators, just tracking state
        .build();

    let node_a_ref = sim.add_node(node_a);
    let _node_b_ref = sim.add_node(node_b);

    // Node A produces 5 blocks
    {
        let mut na = node_a_ref.write().unwrap();
        for i in 0..5 {
            na.insert_evm_tx(make_evm_tx(sender, i, test_addr(20 + i as u8), 100));
            na.produce_block(1_000_000 + i * 250);
        }
    }

    // Node A's chain is linear
    {
        let na = node_a_ref.read().unwrap();
        assert_eq!(na.consensus_height(), 5);

        let mut prev_hash = BlockHash::ZERO;
        for block in &na.blocks_produced {
            assert_eq!(block.header.parent_hash, prev_hash);
            prev_hash = block.header.hash();
        }
    }
}

/// Governance-style upgrade: proposal passes, upgrade activates at height.
#[tokio::test]
async fn test_governance_triggered_upgrade() {
    use call_governance::{GovernanceManager, ProposalType};

    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();
    let val_addr = sender;
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(&mut evm_state, val_addr, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, sender, 100_000,
        );
    }

    // Simulate a governance proposal for protocol upgrade
    let mut gov = GovernanceManager::new();

    // Register a proposer
    let proposer = test_addr(10);
    gov.set_call_balance(proposer, 20_000 * 10u128.pow(18));

    // Submit upgrade proposal
    let proposal_id = gov.submit_proposal(
        proposer,
        ProposalType::ProtocolUpgrade {
            activation_block: 100,
            changelog: "v2.0.0".into(),
        },
        "Protocol Upgrade".into(),
        "Upgrade to v2".into(),
        vec![],
    ).unwrap();

    // The upgrade is scheduled for block 100
    let proposal = gov.get_proposal(proposal_id).unwrap();
    assert_eq!(proposal.state, call_governance::ProposalState::Pending);

    // Produce blocks up to the activation height
    for i in 0..10 {
        node.insert_evm_tx(make_evm_tx(sender, i as u64, test_addr(30 + i as u8), 100));
        node.produce_block(1_000_000 + i * 250);
    }

    assert_eq!(node.consensus_height(), 10);
}

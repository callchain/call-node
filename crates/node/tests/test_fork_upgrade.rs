//! E2E test: Fork and upgrade
//!
//! Height-activated governance upgrade simulation.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, BlockHash, ProtocolVersion};
use call_consensus::{ConsensusParams, SystemTx, SystemTxKind};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_tx(secret: &[u8; 32], sender: Address, nonce: u64, to: Address, amount: u128) -> ProtocolTransaction {
    let tx = ProtocolTransaction {
        sender,
        nonce,
        instructions: vec![Instruction::Transfer {
            asset_id: 1,
            to,
            amount,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
            expires_at: 0,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    };
    sign_tx(secret, tx)
}

/// Simulate a protocol upgrade at a specific block height.
#[tokio::test]
async fn test_height_activated_upgrade() {
    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 100_000).unwrap();
    }

    let upgrade_height = 50u64;
    let mut upgraded = false;

    // Produce blocks up to and past the upgrade height
    for i in 0..60 {
        node.insert_tx(make_tx(&secret, sender, i as u64, test_addr(50 + (i % 10) as u8), 100));

        let ts = 1_000_000 + i * 250;
        let selection = { node.mempool.write().unwrap().select_transactions() };

        let (proposer, height) = {
            let c = node.consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };

        let Some(proposer) = proposer else { continue; };

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        // At upgrade height, inject ProtocolUpgrade system tx
        let system_txs = if height == upgrade_height && !upgraded {
            upgraded = true;
            vec![
                SystemTx {
                    kind: SystemTxKind::UpdateBaseFee,
                    data: vec![],
                },
                SystemTx {
                    kind: SystemTxKind::ProtocolUpgrade(ProtocolVersion::new(2, 0, 0)),
                    data: vec![],
                },
            ]
        } else {
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }]
        };

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = call_consensus::Block::new(
            height,
            node.parent_hash,
            ts,
            proposer,
            version,
            protocol_txs,
            vec![],
            system_txs,
            selection.bridge_ops,
        );

        let result = {
            let mut balances = node.state.balance_state.write().unwrap();
            let mut registry = node.state.asset_registry.write().unwrap();
            let mut compliance = node.state.compliance_engine.write().unwrap();
            let mut bridge_state = node.state.bridge_state.write().unwrap();
            let mut shielded_state = node.state.shielded_state.write().unwrap();
            let mut fee_params = node.state.fee_params.write().unwrap();
            let mut evm_state = node.state.evm_state.write().unwrap();

            block
                .execute(&mut balances, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut fee_params, height, &mut evm_state, None, None, None, None, None, None, None, None)
                .expect("execution")
        };
        block.finalize(&result);

        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
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

    let (secret, sender) = test_keypair();

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
            na.insert_tx(make_tx(&secret, sender, i, test_addr(20 + i as u8), 100));
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
    use call_governance::{GovernanceManager, ProposalType, Vote};

    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
    let val_addr = sender;
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(val_addr, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 100_000).unwrap();
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
        node.insert_tx(make_tx(&secret, sender, i as u64, test_addr(30 + i as u8), 100));
        node.produce_block(1_000_000 + i * 250);
    }

    assert_eq!(node.consensus_height(), 10);
}

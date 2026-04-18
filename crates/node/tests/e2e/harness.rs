//! E2E test harness — deterministic simulation of Callchain nodes.
//!
//! Provides `TestNode`, `NetworkSimulator`, and `NodeBuilder` for
//! constructing end-to-end test scenarios.

use call_consensus::{Block, ConsensusParams, SimplexConsensus, SystemTx, SystemTxKind};
use call_network::{InMemoryNetwork, Network, NetworkMessage, BlockAnnouncement};
use call_primitives::{Address, BlockHash, Ed25519PublicKey, ValidatorId};
use call_protocol::{
    BalanceState, AssetRegistry, ComplianceEngine,
    instructions::Instruction,
    transaction::{AuthScheme, FeeParams, GasConfig, ProtocolTransaction},
};
use call_oracle::OracleManager;
use call_transaction_pool::Mempool;
use call_rpc::RpcState;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ── Constants ─────────────────────────────────────────────────────────

const TX_CHANNEL: u64 = 1;
const BLOCK_CHANNEL: u64 = 2;

// ── Helpers ───────────────────────────────────────────────────────────

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn test_pubkey(n: u8) -> Ed25519PublicKey {
    let mut key = [0u8; 32];
    key[0] = n;
    key
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

// ── NodeBuilder ───────────────────────────────────────────────────────

/// Builder for constructing a [`TestNode`] with custom configuration.
pub struct NodeBuilder {
    data_dir: Option<PathBuf>,
    chain_id: u64,
    validator_stakes: Vec<(Address, Ed25519PublicKey, u128)>,
    initial_balances: Vec<(u64, Address, u128)>, // (asset_id, addr, amount)
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self {
            data_dir: None,
            chain_id: 1337,
            validator_stakes: Vec::new(),
            initial_balances: Vec::new(),
        }
    }
}

impl NodeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn data_dir(mut self, dir: PathBuf) -> Self {
        self.data_dir = Some(dir);
        self
    }

    pub fn chain_id(mut self, id: u64) -> Self {
        self.chain_id = id;
        self
    }

    pub fn validator(mut self, addr: Address, pubkey: Ed25519PublicKey, stake: u128) -> Self {
        self.validator_stakes.push((addr, pubkey, stake));
        self
    }

    pub fn balance(mut self, asset_id: u64, addr: Address, amount: u128) -> Self {
        self.initial_balances.push((asset_id, addr, amount));
        self
    }

    pub fn build(self) -> TestNode {
        let data_dir = self.data_dir.unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "call-e2e-{}-{}",
                std::process::id(),
                rand_id()
            ))
        });

        let mempool = Arc::new(RwLock::new(Mempool::new()));

        let mut consensus = SimplexConsensus::new(
            ConsensusParams::default(),
            call_consensus::ValidatorStateManager::default(),
        );

        for (addr, pubkey, stake) in &self.validator_stakes {
            consensus.stake_validator(*addr, *pubkey, *stake).expect("stake validator");
        }
        consensus.refresh_proposer_subset();

        let state = Arc::new(RpcState::new(
            BalanceState::new(),
            AssetRegistry::new(),
            ComplianceEngine::new(),
            call_evm::EvmState::new(),
            call_bridge::BridgeStateManager::default(),
            call_consensus::ValidatorStateManager::default(),
            call_agent::AgentRegistry::new(),
            call_agent::AgentBalances::new(),
            call_shielded::ShieldedState::new(),
            mempool.clone(),
            self.chain_id,
            OracleManager::default(),
        ));

        for (asset_id, addr, amount) in &self.initial_balances {
            state
                .balance_state
                .write()
                .unwrap()
                .balances
                .set_balance(*asset_id, *addr, *amount)
                .expect("set balance");
        }

        TestNode {
            state,
            mempool,
            consensus: Arc::new(RwLock::new(consensus)),
            network: None,
            parent_hash: BlockHash::ZERO,
            data_dir,
            height: 0,
            blocks_produced: Vec::new(),
        }
    }
}

// ── TestNode ──────────────────────────────────────────────────────────

/// A simulated Callchain node for E2E testing.
///
/// Encapsulates consensus + execution layer. Supports deterministic
/// block production via `produce_block()`.
pub struct TestNode {
    pub state: Arc<RpcState>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub consensus: Arc<RwLock<SimplexConsensus>>,
    pub network: Option<Arc<dyn Network>>,
    pub parent_hash: BlockHash,
    pub data_dir: PathBuf,
    pub height: u64,
    pub blocks_produced: Vec<Block>,
}

impl TestNode {
    /// Create a node with the default builder.
    pub fn new() -> Self {
        NodeBuilder::new().build()
    }

    /// Create a node from a builder.
    pub fn from_builder(builder: NodeBuilder) -> Self {
        builder.build()
    }

    /// Inject a network implementation for P2P testing.
    pub fn inject_network(&mut self, network: Arc<dyn Network>) {
        self.network = Some(Arc::clone(&network));
    }

    /// Insert a protocol transaction into the mempool.
    pub fn insert_tx(&self, tx: ProtocolTransaction) {
        let mut mempool = self.mempool.write().unwrap();
        let _ = mempool.insert_protocol_tx(tx);
    }

    /// Produce a single block deterministically.
    ///
    /// Returns the produced block, or `None` if no proposer is available.
    pub fn produce_block(&mut self, timestamp: u64) -> Option<Block> {
        // Select transactions
        let selection = { self.mempool.write().unwrap().select_transactions() };

        let (proposer, height) = {
            let c = self.consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };
        let Some(proposer) = proposer else {
            return None;
        };

        // Deserialize protocol txs
        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let mut block = Block::new(
            height,
            self.parent_hash,
            timestamp,
            proposer,
            protocol_txs,
            evm_txs,
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // Execute
        let result = {
            let mut balances = self.state.balance_state.write().unwrap();
            let registry = self.state.asset_registry.read().unwrap();
            let mut compliance = self.state.compliance_engine.write().unwrap();
            let mut bridge_state = self.state.bridge_state.write().unwrap();
            let mut shielded_state = self.state.shielded_state.write().unwrap();
            let mut fee_params = self.state.fee_params.write().unwrap();
            let mut evm_state = self.state.evm_state.write().unwrap();
            let mut oracle = self.state.oracle.write().unwrap();

            block
                .execute(
                    &mut balances,
                    &registry,
                    &mut compliance,
                    &mut bridge_state,
                    &mut shielded_state,
                    &mut fee_params,
                    height,
                    &mut evm_state,
                    Some(&mut *oracle),
                )
                .expect("block execution")
        };
        block.finalize(&result);

        // Commit
        {
            let mut consensus = self.consensus.write().unwrap();
            consensus.commit_block(&block, &result, None).expect("commit block");
        }

        let new_height = height + 1;
        self.state.set_current_block(new_height);
        self.parent_hash = block.header.hash();
        self.state.finalize_block();
        self.height = new_height;
        self.blocks_produced.push(block.clone());

        // Broadcast if network is available
        if let Some(ref net) = self.network {
            let announcement = BlockAnnouncement {
                block_hash: self.parent_hash,
                height,
                proposer,
                timestamp_millis: block.header.timestamp_millis,
            };
            let msg = serde_json::to_vec(&NetworkMessage::BlockAnnouncement(announcement))
                .expect("serialize block announcement");
            // Spawn broadcast so it doesn't block
            let net_clone = Arc::clone(net);
            tokio::spawn(async move {
                net_clone.broadcast(BLOCK_CHANNEL, msg).await;
            });
        }

        Some(block)
    }

    /// Produce N blocks in sequence.
    pub fn produce_blocks(&mut self, count: u64, base_timestamp: u64) -> Vec<Block> {
        let mut blocks = Vec::with_capacity(count as usize);
        for i in 0..count {
            let ts = base_timestamp + i * 250;
            if let Some(block) = self.produce_block(ts) {
                blocks.push(block);
            }
        }
        blocks
    }

    /// Get current consensus height.
    pub fn consensus_height(&self) -> u64 {
        self.consensus.read().unwrap().current_height()
    }

    /// Get current base fee.
    pub fn base_fee(&self) -> u128 {
        self.state.fee_params.read().unwrap().base_fee
    }

    /// Get a balance for an address.
    pub fn balance(&self, asset_id: u64, addr: &Address) -> u128 {
        self.state.balance_state.read().unwrap().get_balance(asset_id, addr)
    }

    /// Get mempool size (protocol txs).
    pub fn mempool_size(&self) -> usize {
        self.mempool.read().unwrap().protocol_pool.len()
    }

    /// Persist the latest block to disk.
    pub fn persist_latest_block(&self) -> Result<(), String> {
        if let Some(block) = self.blocks_produced.last() {
            let height = block.header.height;
            let dir = self.data_dir.join("blocks");
            std::fs::create_dir_all(&dir).map_err(|e| format!("create dir: {e}"))?;
            let path = dir.join(format!("{height:012}.json"));
            let data = serde_json::to_vec(block).map_err(|e| format!("serialize: {e}"))?;
            std::fs::write(&path, data).map_err(|e| format!("write: {e}"))?;
        }
        Ok(())
    }

    /// Cleanup data directory.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

impl Drop for TestNode {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ── NetworkSimulator ──────────────────────────────────────────────────

/// Simulates a P2P network connecting multiple [`TestNode`] instances.
///
/// Uses `InMemoryNetwork` as the backing transport. Messages broadcast
/// by one node are available for other nodes to receive.
pub struct NetworkSimulator {
    network: Arc<InMemoryNetwork>,
    nodes: Vec<Arc<RwLock<TestNode>>>,
}

impl NetworkSimulator {
    pub fn new() -> Self {
        Self {
            network: Arc::new(InMemoryNetwork::new()),
            nodes: Vec::new(),
        }
    }

    /// Add a node to the simulated network.
    pub fn add_node(&mut self, mut node: TestNode) -> Arc<RwLock<TestNode>> {
        node.inject_network(Arc::clone(&self.network) as Arc<dyn Network>);
        let wrapped = Arc::new(RwLock::new(node));
        self.nodes.push(Arc::clone(&wrapped));
        wrapped
    }

    /// Get the shared network.
    pub fn network(&self) -> Arc<InMemoryNetwork> {
        Arc::clone(&self.network)
    }

    /// Get a node by index.
    pub fn node(&self, idx: usize) -> Arc<RwLock<TestNode>> {
        Arc::clone(&self.nodes[idx])
    }

    /// Get the number of connected nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Receive the next broadcast message from the network.
    pub async fn receive_broadcast(&self) -> Option<(String, u64, Vec<u8>)> {
        self.network.receive().await.ok()
    }

    /// Drain all pending messages from the network buffer.
    pub async fn drain_messages(&self) -> Vec<(String, u64, Vec<u8>)> {
        let mut msgs = Vec::new();
        while let Ok(msg) = self.network.receive().await {
            msgs.push(msg);
        }
        msgs
    }
}

// ── DeterministicRuntime ──────────────────────────────────────────────

/// Runs consensus in a deterministic simulation mode.
///
/// Instead of real time-based block production, this advances blocks
/// on demand with a monotonically increasing timestamp.
pub struct DeterministicRuntime {
    pub simulator: NetworkSimulator,
    pub timestamp: u64,
}

impl DeterministicRuntime {
    pub fn new() -> Self {
        Self {
            simulator: NetworkSimulator::new(),
            timestamp: 1_000_000, // start at a non-zero timestamp
        }
    }

    /// Add a fresh node with optional validator stake.
    pub fn add_validator_node(
        &mut self,
        validator_id: u8,
        stake: u128,
    ) -> Arc<RwLock<TestNode>> {
        let node = NodeBuilder::new()
            .validator(
                test_addr(validator_id),
                test_pubkey(validator_id),
                stake,
            )
            .build();
        self.simulator.add_node(node)
    }

    /// Add a node with initial balances.
    pub fn add_node_with_balance(
        &mut self,
        addr: Address,
        amount: u128,
    ) -> Arc<RwLock<TestNode>> {
        let node = NodeBuilder::new()
            .balance(0, addr, amount)
            .build();
        self.simulator.add_node(node)
    }

    /// Produce one block on the first node that has a proposer.
    pub fn produce_one_block(&mut self) -> Option<Block> {
        self.timestamp += 250;
        if let Ok(mut node) = self.simulator.node(0).write() {
            node.produce_block(self.timestamp)
        } else {
            None
        }
    }

    /// Produce N blocks.
    pub fn produce_blocks(&mut self, count: u64) -> Vec<Block> {
        let mut blocks = Vec::new();
        for _ in 0..count {
            if let Some(block) = self.produce_one_block() {
                blocks.push(block);
            }
        }
        blocks
    }
}

// ── SharedTxCorpus ────────────────────────────────────────────────────

/// A shared pool of transactions that can be injected into multiple nodes.
pub struct SharedTxCorpus {
    pub transactions: Vec<ProtocolTransaction>,
    next_nonce: u64,
}

impl SharedTxCorpus {
    pub fn new() -> Self {
        Self {
            transactions: Vec::new(),
            next_nonce: 0,
        }
    }

    /// Create a simple transfer transaction.
    pub fn make_transfer(
        &mut self,
        sender: Address,
        receiver: Address,
        asset_id: u64,
        amount: u128,
    ) -> ProtocolTransaction {
        let tx = ProtocolTransaction {
            sender,
            nonce: self.next_nonce,
            instructions: vec![Instruction::Transfer {
                asset_id,
                to: receiver,
                amount,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        self.next_nonce += 1;
        self.transactions.push(tx.clone());
        tx
    }

    /// Create a batch of transfers from the same sender.
    pub fn make_transfers(
        &mut self,
        sender: Address,
        transfers: Vec<(Address, u64, u128)>, // (receiver, asset_id, amount)
    ) -> Vec<ProtocolTransaction> {
        transfers
            .into_iter()
            .map(|(receiver, asset_id, amount)| {
                self.make_transfer(sender, receiver, asset_id, amount)
            })
            .collect()
    }

    /// Inject all transactions into a node's mempool.
    pub fn inject_into(&self, node: &TestNode) {
        for tx in &self.transactions {
            node.insert_tx(tx.clone());
        }
    }
}

// ── Utility ───────────────────────────────────────────────────────────

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

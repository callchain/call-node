//! E2E test harness — deterministic simulation of Callchain nodes.
//!
//! Provides `TestNode`, `NetworkSimulator`, and `NodeBuilder` for
//! constructing end-to-end test scenarios.

#![allow(dead_code, unreachable_pub)]

use call_consensus::{Block, BlockExecutionResult, ConsensusParams, SimplexConsensus};
use call_network::{InMemoryNetwork, Network, NetworkMessage, BlockAnnouncement};
use call_primitives::{Address, BlockHash, Ed25519PublicKey, TxHash};
use call_transaction_pool::Mempool;
use call_governance::GovernanceManager;
use call_rpc::{RpcState, wire_governance_executor};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ── Signing helpers for E2E tests ─────────────────────────────────────

/// Generate a random secp256k1 keypair and derive the Ethereum address.
pub fn test_keypair() -> ([u8; 32], Address) {
    let (secret, _) = call_crypto::generate_keypair();
    let msg_hash = [0u8; 32];
    let sig = call_crypto::secp256k1_sign(&secret, &msg_hash);
    let addr = call_crypto::recover_secp256k1_signer(&msg_hash, &sig).unwrap();
    (secret, addr)
}

// ── Constants ─────────────────────────────────────────────────────────

const TX_CHANNEL: u64 = 1;
const BLOCK_CHANNEL: u64 = 2;

// ── Helpers ───────────────────────────────────────────────────────────

pub fn test_addr(n: u8) -> Address {
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

        let mut evm_state = call_evm::EvmState::new();
        let mut consensus = SimplexConsensus::new(
            ConsensusParams::default(),
            &evm_state,
        );

        for (addr, pubkey, stake) in &self.validator_stakes {
            consensus.stake_validator(&mut evm_state, *addr, *pubkey, *stake).expect("stake validator");
        }
        consensus.refresh_proposer_subset(&evm_state);

        let state = Arc::new(RpcState::new(
            call_evm::EvmState::new(),
            call_agent::AgentNonces::new(),
            mempool.clone(),
            self.chain_id,
        ));

        for (asset_id, addr, amount) in &self.initial_balances {
            let mut evm = state.evm_state.write().unwrap();
            call_consensus::exec::state_accessors::seed_balance(
                &mut *evm, *asset_id, *addr, *amount,
            );
        }

        let mut governance = GovernanceManager::new();
        wire_governance_executor(&mut governance, &state);
        let governance = Arc::new(RwLock::new(governance));

        TestNode {
            state,
            mempool,
            consensus: Arc::new(RwLock::new(consensus)),
            network: None,
            parent_hash: BlockHash::ZERO,
            data_dir,
            height: 0,
            blocks_produced: Vec::new(),
            last_result: None,
            governance,
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
    /// The execution result of the most recent `produce_block()` call.
    pub last_result: Option<BlockExecutionResult>,
    /// Governance state machine (sidecar, not in RpcState)
    pub governance: Arc<RwLock<GovernanceManager>>,
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

    /// Insert an EVM transaction into the mempool.
    pub fn insert_evm_tx(&self, tx: call_evm::EvmTransaction) {
        let mut mempool = self.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx);
    }

    /// Produce a single block deterministically.
    ///
    /// Returns the produced block, or `None` if no proposer is available.
    pub fn produce_block(&mut self, timestamp: u64) -> Option<Block> {
        // Select transactions (EVM-only)
        let selection = { self.mempool.write().unwrap().select_transactions() };

        let (proposer, height) = {
            let c = self.consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };
        let Some(proposer) = proposer else {
            return None;
        };

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let version = self.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            self.parent_hash,
            timestamp,
            proposer,
            version,
            evm_txs,
        );

        // Execute
        {
            let mut gov = self.governance.write().unwrap();
            gov.set_current_block(height);
        }
        let result = {
            let mut s = self.state.write_all();
            s.execute_block(&block, height)
                .expect("block execution")
        };
        block.finalize(&result);

        // Commit
        {
            let mut evm_state = self.state.evm_state.write().unwrap();
            let mut consensus = self.consensus.write().unwrap();
            consensus.commit_block(&block, &result, &mut evm_state).expect("commit block");
        }
        self.last_result = Some(result.clone());

        // Remove confirmed transactions from mempool (only executed txs)
        {
            let evm_hashes: Vec<TxHash> = result.evm_tx_results.iter()
                .map(|r| r.tx_hash)
                .collect();
            let mut mp = self.mempool.write().unwrap();
            mp.confirm_transactions(&evm_hashes);
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
            let msg = bincode::serialize(&NetworkMessage::BlockAnnouncement(announcement))
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
        use call_consensus::exec::state_accessors;
        let evm = self.state.evm_state.read().unwrap();
        state_accessors::read_balance(&*evm, asset_id, *addr)
    }

    /// Get mempool size (EVM txs).
    pub fn mempool_size(&self) -> usize {
        self.mempool.read().unwrap().evm_pool.len()
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

/// A shared pool of EVM transactions that can be injected into multiple nodes.
pub struct SharedTxCorpus {
    pub transactions: Vec<call_evm::EvmTransaction>,
    next_nonce: u64,
}

impl SharedTxCorpus {
    pub fn new() -> Self {
        Self {
            transactions: Vec::new(),
            next_nonce: 0,
        }
    }

    /// Create a simple EVM transfer transaction.
    pub fn make_transfer(
        &mut self,
        sender: Address,
        receiver: Address,
        _asset_id: u64,
        amount: u128,
    ) -> call_evm::EvmTransaction {
        let tx = call_evm::EvmTransaction {
            caller: sender,
            nonce: self.next_nonce,
            gas_limit: 21_000,
            gas_price: 1_000_000_000,
            to: Some(receiver),
            value: call_primitives::U256::from(amount),
            data: call_evm::Bytes::default(),
            chain_id: 1,
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
    ) -> Vec<call_evm::EvmTransaction> {
        transfers
            .into_iter()
            .map(|(receiver, _asset_id, amount)| {
                self.make_transfer(sender, receiver, 0, amount)
            })
            .collect()
    }

    /// Inject all transactions into a node's mempool.
    pub fn inject_into(&self, node: &TestNode) {
        for tx in &self.transactions {
            node.insert_evm_tx(tx.clone());
        }
    }
}

// ── Utility ───────────────────────────────────────────────────────────

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

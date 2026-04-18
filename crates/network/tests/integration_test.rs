//! Integration tests for CommonwareNetwork.
//!
//! Tests the real P2P network layer with localhost TCP sockets,
//! verifying peer connection, message passing, and network health.

use call_network::{
    CommonwareConfig, CommonwareNetwork, Network, NetworkLimits,
    load_or_generate_identity_key, NetworkMessage, TransactionMessage,
};
use call_primitives::TxHash;
use commonware_codec::extensions::DecodeExt;
use commonware_cryptography::ed25519;
use commonware_cryptography::Signer;
use std::net::TcpListener;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Find available localhost ports for testing
fn find_available_ports(count: usize) -> Vec<SocketAddr> {
    let mut ports = Vec::with_capacity(count);
    for _ in 0..count {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind");
        let addr = listener.local_addr().expect("failed to get local addr");
        drop(listener);
        ports.push(addr);
    }
    ports
}

/// Create a temporary data directory for a test node
fn temp_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("call-network-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// Generate a random ed25519 keypair for testing
fn test_key() -> ed25519::PrivateKey {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    ed25519::PrivateKey::decode(&seed[..]).expect("valid key")
}

/// Test that two CommonwareNetwork instances can connect and exchange messages
#[tokio::test]
async fn test_two_node_connection_and_message() {
    let ports = find_available_ports(2);
    let node1_addr = ports[0];
    let node2_addr = ports[1];

    let key1 = test_key();
    let key2 = test_key();

    let _dir1 = temp_data_dir("node1");
    let _dir2 = temp_data_dir("node2");

    // Start Node 1 (no bootstrap peers, just listens)
    let config1 = CommonwareConfig {
        listen_addr: node1_addr,
        bootstrap_peers: Vec::new(),
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 0,
        limits: NetworkLimits::default(),
    };
    let mut node1 = CommonwareNetwork::new(&config1, key1).await.expect("node1 init");

    let node1_peer_id = node1.peer_id().to_string();

    // Start Node 2 with node1 as bootstrap peer
    let config2 = CommonwareConfig {
        listen_addr: node2_addr,
        bootstrap_peers: vec![(node1_peer_id.clone(), node1_addr)],
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 0,
        limits: NetworkLimits::default(),
    };
    let mut node2 = CommonwareNetwork::new(&config2, key2).await.expect("node2 init");

    // Node 1 also tracks node2 (mutual connection for bidirectional peer visibility)
    let node2_peer_id = node2.peer_id().to_string();
    node1.connect(&format!("{node2_peer_id}@{node2_addr}")).await.expect("node1 connect to node2");

    // Wait for peer discovery to process
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Verify both nodes see peers (node2 tracks node1 via bootstrap, node1 tracks node2 via connect)
    let peer_count_1 = node1.peer_count();
    let peer_count_2 = node2.peer_count();
    assert!(peer_count_1 >= 1, "node1 should see at least 1 peer, saw {peer_count_1}");
    assert!(peer_count_2 >= 1, "node2 should see at least 1 peer, saw {peer_count_2}");

    // Node 1 broadcasts a transaction message
    let tx_data = vec![0x01, 0x02, 0x03];
    let tx_hash = TxHash::repeat_byte(0xAB);
    let tx_msg = TransactionMessage::new(tx_data.clone(), tx_hash);
    let network_msg = NetworkMessage::Transaction(tx_msg);
    let wire_data = bincode::serialize(&network_msg).expect("serialize");

    node1.broadcast(1, wire_data.clone()).await;

    // Node 2 receives the message
    let (_sender_id, channel, data) = tokio::time::timeout(
        Duration::from_secs(5),
        node2.receive(),
    )
    .await
    .expect("receive timeout")
    .expect("receive failed");

    assert_eq!(channel, 1, "channel should match");

    let received_msg: NetworkMessage = bincode::deserialize(&data).expect("deserialize");
    assert!(matches!(received_msg, NetworkMessage::Transaction(_)));

    // Cleanup
    node1.stop();
    node2.stop();
}

/// Test network health check with minimum peer requirements
#[tokio::test]
async fn test_network_health_check() {
    let ports = find_available_ports(2);

    let key1 = test_key();
    let key2 = test_key();

    // Both nodes start with each other as bootstrap (mutual tracking)
    let node1_peer_id = hex::encode(key1.public_key().as_ref());
    let node2_peer_id = hex::encode(key2.public_key().as_ref());

    let config1 = CommonwareConfig {
        listen_addr: ports[0],
        bootstrap_peers: vec![(node2_peer_id.clone(), ports[1])],
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 1,
        limits: NetworkLimits::default(),
    };
    let mut node1 = CommonwareNetwork::new(&config1, key1).await.expect("node1 init");

    let config2 = CommonwareConfig {
        listen_addr: ports[1],
        bootstrap_peers: vec![(node1_peer_id.clone(), ports[0])],
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 1,
        limits: NetworkLimits::default(),
    };
    let mut node2 = CommonwareNetwork::new(&config2, key2).await.expect("node2 init");

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Both nodes should be healthy (each tracks at least 1 peer)
    assert!(node1.is_healthy(), "node1 should be healthy (peers: {})", node1.peer_count());
    assert!(node2.is_healthy(), "node2 should be healthy (peers: {})", node2.peer_count());

    node1.stop();
    node2.stop();
}

/// Test peer disconnect removes from peer tracking
#[tokio::test]
async fn test_disconnect_removes_peer() {
    let ports = find_available_ports(2);

    let key1 = test_key();
    let key2 = test_key();

    let node1_peer_id = hex::encode(key1.public_key().as_ref());
    let node2_peer_id = hex::encode(key2.public_key().as_ref());

    // Both nodes track each other
    let config1 = CommonwareConfig {
        listen_addr: ports[0],
        bootstrap_peers: vec![(node2_peer_id.clone(), ports[1])],
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 0,
        limits: NetworkLimits::default(),
    };
    let mut node1 = CommonwareNetwork::new(&config1, key1).await.expect("node1 init");

    let config2 = CommonwareConfig {
        listen_addr: ports[1],
        bootstrap_peers: vec![(node1_peer_id.clone(), ports[0])],
        max_message_size: 1024 * 1024,
        allow_private_ips: true,
        namespace: b"callchain-test".to_vec(),
        min_healthy_peers: 0,
        limits: NetworkLimits::default(),
    };
    let mut node2 = CommonwareNetwork::new(&config2, key2).await.expect("node2 init");

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Verify node1 sees node2
    let initial_count = node1.peer_count();
    assert!(initial_count >= 1, "node1 should have at least 1 peer, saw {initial_count}");

    // Disconnect node2 from node1's perspective
    node1.disconnect(&node2_peer_id).await.expect("disconnect failed");

    // Peer should be removed from node1's tracking
    let after_count = node1.peer_count();
    assert!(after_count < initial_count, "peer count should decrease (was {initial_count}, now {after_count})");

    node1.stop();
    node2.stop();
}

/// Test identity key persistence across restarts
#[tokio::test]
async fn test_identity_key_persistence() {
    let dir = temp_data_dir("identity");

    // First run: generate and persist key
    let key1 = load_or_generate_identity_key(&dir, None).expect("generate key1");
    let peer_id_1 = hex::encode(key1.public_key().as_ref());

    // Second run: should load the same key
    let key2 = load_or_generate_identity_key(&dir, None).expect("generate key2");
    let peer_id_2 = hex::encode(key2.public_key().as_ref());

    assert_eq!(peer_id_1, peer_id_2, "peer IDs should match across restarts");

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}

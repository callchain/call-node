//! Unit tests for the P2P layer.

use super::super::limits::NetworkLimits;
use super::super::p2p::message::*;
use super::super::p2p::wire::{decode_with_channel, encode_with_channel};
use super::super::p2p::config::CommonwareConfig;
use super::super::p2p::memory::InMemoryNetwork;
use super::super::p2p::trait_::Network;
use call_primitives::{PricePair, TxHash};

fn test_hash(n: u8) -> TxHash {
    TxHash::repeat_byte(n)
}

#[test]
fn test_transaction_message_checksum() {
    let data = vec![1, 2, 3, 4, 5];
    let hash = test_hash(1);
    let msg = TransactionMessage::new(data.clone(), hash);

    assert!(msg.verify_checksum());
    assert_eq!(msg.hash, hash);
    assert_eq!(msg.data, data);
}

#[test]
fn test_transaction_message_corrupted_data() {
    let data = vec![1, 2, 3, 4, 5];
    let hash = test_hash(1);
    let mut msg = TransactionMessage::new(data, hash);

    // Corrupt data
    msg.data[0] = 0xFF;
    assert!(!msg.verify_checksum());
}

#[test]
fn test_block_announcement() {
    let ann = BlockAnnouncement {
        block_hash: test_hash(0xAB),
        height: 100,
        proposer: 42,
        timestamp_millis: 1_000_000,
    };

    assert_eq!(ann.height, 100);
    assert_eq!(ann.proposer, 42);
}

#[test]
fn test_sync_request_response() {
    let req = SyncRequest {
        start_height: 10,
        count: 5,
        full_state: true,
    };

    assert_eq!(req.start_height, 10);
    assert_eq!(req.count, 5);
    assert!(req.full_state);
}

#[test]
fn test_handshake() {
    let hs = Handshake {
        version: 1,
        chain_id: 1,
        best_height: 1000,
        best_hash: test_hash(0xFF),
        capabilities: 0b111, // tx + block + sync
    };

    assert_eq!(hs.version, 1);
    assert_eq!(hs.chain_id, 1);
}

#[test]
fn test_oracle_price_request() {
    let req = OraclePriceRequest {
        pairs: vec![PricePair::new(1, 0), PricePair::new(2, 0), PricePair::new(3, 0)],
        block: 1000,
        requester_id: 5,
    };
    assert_eq!(req.pairs.len(), 3);
    assert_eq!(req.block, 1000);
    assert_eq!(req.requester_id, 5);

    // Test serialization
    let msg = NetworkMessage::OraclePriceRequest(req.clone());
    let serialized = bincode::serialize(&msg).unwrap();
    let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
    assert!(matches!(deserialized, NetworkMessage::OraclePriceRequest(r) if r.block == 1000));
}

#[test]
fn test_oracle_price_submission() {
    let sub = OraclePriceSubmission {
        validator_id: 2,
        pair: PricePair::new(1, 0),
        price: 2_000_000,
        block_number: 1000,
        timestamp: 1_000_000,
        signature: [0u8; 64],
        sources: vec!["binance".into()],
    };
    assert_eq!(sub.price, 2_000_000);
    assert_eq!(sub.sources.len(), 1);

    // Test serialization
    let msg = NetworkMessage::OraclePriceSubmission(sub.clone());
    let serialized = bincode::serialize(&msg).unwrap();
    let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
    assert!(matches!(deserialized, NetworkMessage::OraclePriceSubmission(s) if s.price == 2_000_000));
}

#[test]
fn test_network_event_oracle_variants() {
    use super::super::p2p::event::NetworkEvent;

    let evt = NetworkEvent::OraclePriceRequestReceived {
        peer_id: "peer_1".into(),
        request: OraclePriceRequest {
            pairs: vec![PricePair::new(1, 0)],
            block: 1000,
            requester_id: 0,
        },
    };
    assert!(matches!(evt, NetworkEvent::OraclePriceRequestReceived { .. }));

    let evt2 = NetworkEvent::OraclePriceSubmissionReceived {
        peer_id: "peer_2".into(),
        submission: OraclePriceSubmission {
            validator_id: 1,
            pair: PricePair::new(1, 0),
            price: 100,
            block_number: 1000,
            timestamp: 1000,
            signature: [0u8; 64],
            sources: vec![],
        },
    };
    assert!(matches!(evt2, NetworkEvent::OraclePriceSubmissionReceived { .. }));
}

#[test]
fn test_network_event_variants() {
    use super::super::p2p::event::NetworkEvent;

    let evt1 = NetworkEvent::PeerConnected {
        peer_id: "peer_1".into(),
    };
    assert!(matches!(evt1, NetworkEvent::PeerConnected { .. }));

    let evt2 = NetworkEvent::TransactionReceived {
        peer_id: "peer_1".into(),
        hash: test_hash(1),
        data: vec![1, 2, 3],
    };
    assert!(matches!(evt2, NetworkEvent::TransactionReceived { .. }));

    let evt3 = NetworkEvent::BlockAnnouncementReceived {
        peer_id: "peer_1".into(),
        announcement: BlockAnnouncement {
            block_hash: test_hash(0xAB),
            height: 100,
            proposer: 1,
            timestamp_millis: 1000,
        },
    };
    assert!(matches!(
        evt3,
        NetworkEvent::BlockAnnouncementReceived { .. }
    ));
}

#[test]
fn test_crc32_deterministic() {
    use super::super::p2p::message::crc32_fast;

    let data = b"hello world";
    let crc1 = crc32_fast(data);
    let crc2 = crc32_fast(data);
    assert_eq!(crc1, crc2);
}

#[test]
fn test_crc32_different_data() {
    use super::super::p2p::message::crc32_fast;

    let crc1 = crc32_fast(b"hello");
    let crc2 = crc32_fast(b"world");
    assert_ne!(crc1, crc2);
}

#[test]
fn test_network_limits_integration() {
    let limits = NetworkLimits::default();
    assert_eq!(limits.max_peers, 50);
    assert_eq!(limits.max_messages_per_second, 10_000);
}

#[tokio::test]
async fn test_in_memory_network_connect_disconnect() {
    let network = InMemoryNetwork::new();
    assert_eq!(network.peer_count(), 0);
    assert!(!network.is_healthy());

    network.connect("peer_1").await.unwrap();
    assert_eq!(network.peer_count(), 1);
    assert!(network.is_healthy());

    network.disconnect("peer_1").await.unwrap();
    assert_eq!(network.peer_count(), 0);
    assert!(!network.is_healthy());
}

#[tokio::test]
async fn test_in_memory_network_broadcast() {
    let network = InMemoryNetwork::new();
    network.connect("peer_1").await.unwrap();
    network.broadcast(1, vec![1, 2, 3]).await;

    let (peer_id, channel, data) = network.receive().await.unwrap();
    assert_eq!(peer_id, "broadcast");
    assert_eq!(channel, 1);
    assert_eq!(data, vec![1, 2, 3]);
}

#[tokio::test]
async fn test_in_memory_network_receive_empty() {
    let network = InMemoryNetwork::new();
    let result = network.receive().await;
    assert!(result.is_err());
}

#[test]
fn test_wire_protocol_channel_encoding() {
    let channel = 42u64;
    let payload = vec![1, 2, 3, 4, 5];
    let encoded = encode_with_channel(channel, &payload);

    assert_eq!(encoded[0], 42);
    assert_eq!(&encoded[1..], &payload);

    let (decoded_channel, decoded_payload) = decode_with_channel(&encoded).unwrap();
    assert_eq!(decoded_channel, channel);
    assert_eq!(decoded_payload, payload.as_slice());
}

#[test]
fn test_wire_protocol_empty_message() {
    assert!(decode_with_channel(&[]).is_none());
}

#[test]
fn test_commonware_config_defaults() {
    let cfg = CommonwareConfig::default();
    assert!(!cfg.allow_private_ips);
    assert_eq!(cfg.namespace, b"callchain");
}

#[test]
fn test_commonware_config_local() {
    let addr = "127.0.0.1:51235".parse().unwrap();
    let cfg = CommonwareConfig::local(addr);
    assert!(cfg.allow_private_ips);
    assert_eq!(cfg.listen_addr, addr);
}

#[test]
fn test_peer_exchange_serialization() {
    let pex = PeerExchange::new(
        vec![
            ("abcd".to_string(), "127.0.0.1:5001".parse().unwrap()),
            ("efgh".to_string(), "127.0.0.1:5002".parse().unwrap()),
        ],
        "127.0.0.1:5000".parse().unwrap(),
    );
    let msg = NetworkMessage::PeerExchange(pex);
    let serialized = bincode::serialize(&msg).unwrap();
    let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
    match deserialized {
        NetworkMessage::PeerExchange(px) => {
            assert_eq!(px.peers.len(), 2);
            assert_eq!(px.sender_addr.to_string(), "127.0.0.1:5000");
        }
        _ => panic!("expected PeerExchange variant"),
    }
}

#[test]
fn test_peer_exchange_truncate() {
    let mut pex = PeerExchange::new(
        (0..100)
            .map(|i| (format!("peer_{i}"), format!("127.0.0.1:{i}").parse().unwrap()))
            .collect(),
        "127.0.0.1:5000".parse().unwrap(),
    );
    pex.truncate(10);
    assert_eq!(pex.peers.len(), 10);
}

#[test]
fn test_commonware_config_pex_defaults() {
    let cfg = CommonwareConfig::default();
    assert!(cfg.enable_peer_exchange);
    assert_eq!(cfg.pex_interval_seconds, 60);
    assert!(!cfg.auto_connect_discovered);
    assert_eq!(cfg.max_known_peers, 1000);
    assert_eq!(cfg.max_pex_peers_per_msg, 50);
}

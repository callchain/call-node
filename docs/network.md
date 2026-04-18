# Callchain Network Layer

## Overview

The Callchain P2P network layer (`crates/network/`) provides authenticated peer-to-peer communication for the Callchain node. It is built on **commonware-p2p** (authenticated lookup network with Ed25519 identities) and wraps it with gossip-based transaction propagation, rate limiting, and state sync.

```
┌─────────────────────────────────────────────────┐
│                   CallNode                       │
│  ┌──────────┐  ┌──────────┐  ┌───────────────┐  │
│  │ Consensus │  │   RPC    │  │ State Manager  │  │
│  └────┬─────┘  └────┬─────┘  └───────┬───────┘  │
│       │              │               │           │
│  ┌────┴──────────────┴───────────────┴───────┐   │
│  │          Network (Arc<dyn Network>)        │   │
│  └────┬──────────────────────────────────────┘   │
└─────┼───────────────────────────────────────────┘
      │
┌─────┼───────────────────────────────────────────┐
│  call-network                                    │
│  ┌──────────────────────────────────────────┐    │
│  │         GossipManager                     │    │
│  │  ┌─────────────┐ ┌──────────┐           │    │
│  │  │ KnownTxsLRU  │ │RateLimit │           │    │
│  │  └─────────────┘ └──────────┘           │    │
│  └─────────────────┬────────────────────────┘    │
│  ┌─────────────────┼────────────────────────┐    │
│  │    CommonwareNetwork / InMemoryNetwork    │    │
│  │  ┌──────────┐ ┌────────┐ ┌────────────┐  │    │
│  │  │  Sender  │ │Receiver│ │   Oracle    │  │    │
│  │  └──────────┘ └────────┘ └────────────┘  │    │
│  └──────────────────────────────────────────┘    │
└──────────────────────────────────────────────────┘
```

## Architecture

### Network Trait

The `Network` trait (`crates/network/src/p2p.rs`) provides an abstraction over the P2P layer, enabling dependency injection for testing:

| Method | Description |
|---|---|
| `broadcast(channel, message)` | Send a message to all connected peers on a given channel |
| `send_to(peers, message)` | Send to specific peers by hex-encoded Ed25519 public key |
| `receive()` | Block until a message arrives; returns `(peer_id, channel, payload)` |
| `peer_count()` | Number of tracked peers |
| `peer_ids()` | List of hex-encoded peer IDs |
| `connect(address)` | Connect to a peer (`"peer_id@host:port"` format) |
| `disconnect(peer_id)` | Block and disconnect a peer |
| `is_healthy()` | Returns true if peer count meets `min_healthy_peers` threshold |

Two implementations exist:
- **`CommonwareNetwork`** — production P2P backed by `commonware-p2p`
- **`InMemoryNetwork`** — in-memory message buffer for unit/e2e tests

### CommonwareNetwork

The real P2P network adapter. Runs the commonware-p2p runtime in a separate OS thread (`std::thread::spawn`) containing a Tokio runtime (`commonware_runtime::tokio`). Communication with the calling thread uses oneshot channels for initialization and a shutdown signal for teardown.

**Key components:**

| Field | Purpose |
|---|---|
| `sender` | commonware-p2p sender for outgoing messages |
| `receiver` | commonware-p2p receiver for incoming messages |
| `oracle` | Peer discovery oracle via `track()`/`block()` |
| `peers` | Shared `BTreeMap<peer_id_hex, SocketAddr>` updated via `PeerSetUpdate` subscription |
| `gossip` | `GossipManager` for per-peer rate limiting and deduplication |
| `bootstrap_peers` | Configured seed peers for periodic reconnection |
| `min_healthy_peers` | Minimum peer count for `is_healthy()` (validators: 1+, full nodes: 0) |

#### Peer Discovery

Callchain uses commonware-p2p's **authenticated lookup** model:

1. **Bootstrap registration** — At startup, configured bootstrap peers are registered via `oracle.track(0, peer_map)` with their Ed25519 public keys and socket addresses.
2. **Peer set subscription** — An `oracle.subscribe()` stream watches for `PeerSetUpdate` events, keeping the local `peers` map in sync with the oracle's view.
3. **Manual connect** — `connect("peer_id@host:port")` decodes the hex public key, constructs an `Address::Symmetric`, and calls `oracle.track()` to initiate discovery.
4. **Disconnect** — `disconnect(peer_id)` decodes the public key and calls `oracle.block()` to prevent future reconnection.
5. **Periodic reconnection** — A background task runs every 30 seconds, re-`track()`ing any bootstrap peers that are no longer in the active peer set.

#### Message Multiplexing

All P2P traffic flows through a single registered application channel (channel 0 in commonware-p2p). Multiplexing is achieved via a **1-byte channel prefix** prepended to each message payload:

```
┌─────┬────────────────────────────────────────────┐
│ 1B  │  Payload (bincode-serialized NetworkMessage)│
│ ch  │                                             │
└─────┴────────────────────────────────────────────┘
```

The channel byte is truncated from the u64 channel ID (`channel as u8`). Application-level channels are:

| Channel ID | Name | Usage |
|---|---|---|
| 1 | `TX_CHANNEL` | Transaction propagation (mempool gossip) |
| 2 | `BLOCK_CHANNEL` | Block announcements and full block relay |
| 3 | `SYNC_CHANNEL` | Request-response state sync |

#### Wire Format

P2P messages use **bincode** for binary serialization. The `NetworkMessage` enum carries all message types:

```rust
pub enum NetworkMessage {
    Transaction(TransactionMessage),    // bincode + inner RLP payload
    BlockAnnouncement(BlockAnnouncement),
    SyncRequest(SyncRequest),
    SyncResponse(SyncResponse),
    Handshake(Handshake),
    OraclePriceRequest(OraclePriceRequest),
    OraclePriceSubmission(OraclePriceSubmission),
}
```

Inner structs (`TransactionMessage`, `BlockAnnouncement`, etc.) derive RLP encode/decode for protocol-layer serialization. The outer envelope uses serde + bincode for the wire.

#### Transaction Integrity

`TransactionMessage` includes a **CRC32 checksum** for data integrity:

```rust
pub struct TransactionMessage {
    pub data: Vec<u8>,     // RLP-encoded transaction
    pub hash: TxHash,      // 32-byte transaction hash
    pub checksum: u32,     // CRC32 of data
}
```

The checksum is computed on construction and verified on receipt to detect corruption.

### GossipManager

Manages transaction propagation with three defenses against P2P attacks:

#### 1. Per-Peer Rate Limiting

Each peer has a **token bucket** rate limiter:

- **max_tokens**: `max_messages_per_second` (default: 100/sec)
- **refill_rate**: tokens per second (same as max)
- **behavior**: Tokens refill continuously based on elapsed time; each message consumes 1 token

When a peer exceeds its rate limit, messages are rejected with `RateLimitExceeded` and the peer can be banned.

#### 2. LRU Deduplication Cache

A fixed-size LRU cache of seen transaction hashes prevents re-processing duplicate transactions:

- **capacity**: `known_txs_cache_size` (default: 1,000,000 entries)
- **eviction**: Oldest entries evicted when at capacity
- **lookup**: O(1) via HashMap + O(1) eviction via VecDeque

#### 3. Peer Management

- **add_peer**: Registers a peer with rate limiter; rejects if at `max_peers` (default: 50)
- **remove_peer**: Cleans up peer state on disconnect
- **ban/unban**: Ban malicious peers with configurable duration (default: 3600s / 1 hour)

### NetworkLimits

Centralized configuration for all network-level protections:

| Parameter | Default | Purpose |
|---|---|---|
| `max_peers` | 50 | Maximum peer connections |
| `max_messages_per_second` | 100 | Per-peer message rate limit |
| `max_message_size` | 10 MB | Maximum individual message size |
| `known_txs_cache_size` | 1,000,000 | Deduplication cache capacity |
| `ban_duration_seconds` | 3600 | Duration for peer bans |

### Node Types and Configuration

Different node modes configure the network differently:

| Setting | Validator | Full Node | Archive |
|---|---|---|---|
| `allow_private_ips` | `false` | `true` | `true` |
| `min_healthy_peers` | 1 | 0 | 0 |
| Validator signing key | Required | Not used | Not used |

## Boot Sequence

The network initializes as part of the node boot sequence (`crates/node/src/boot.rs`, step 4):

1. **Load identity key** — `load_or_generate_identity_key()` with priority:
   - CLI config (`--identity-key`)
   - Persistent file (`{data_dir}/node.key`)
   - Random generation with atomic persistence (tmp file + rename)
2. **Parse bootstrap peers** — From config into `Vec<(peer_id_hex, SocketAddr)>`
3. **Construct config** — `CommonwareConfig` with mode-specific settings
4. **Start network** — `node.start_network(config, identity_key)` spawns the commonware runtime thread and waits for initialization

## Security Model

### Identity Authentication

Each node has a persistent Ed25519 keypair. The public key serves as the **peer ID** and is used for:

- Peer discovery via the oracle
- Message signing/verification (handled by commonware-p2p)
- Replay attack prevention via network namespace binding

The private key is stored as hex in `{data_dir}/node.key`, written atomically via tmp-file + rename to prevent corruption on crash.

### Attack Mitigations

| Attack | Mitigation |
|---|---|
| Transaction flooding | Per-peer token bucket rate limiter |
| Duplicate spam | LRU deduplication cache (1M entries) |
| Oversized messages | `max_message_size` enforcement (10 MB default) |
| Peer exhaustion | `max_peers` cap (50 default) |
| Malicious peers | Ban system with configurable duration |
| Data corruption | CRC32 checksum on transaction messages |
| Network replay | Namespace binding in commonware-p2p config |
| Sybil attacks | Ed25519 identity required for all connections |

### Health Checking

`is_healthy()` returns true only when `peer_count() >= min_healthy_peers`. Validators require at least 1 peer; full/archive nodes accept 0. The health check is used by the boot sequence and monitoring to determine if the node is properly connected to the network.

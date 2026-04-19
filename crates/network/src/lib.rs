//! call-network — P2P network layer for Callchain.
//!
//! Implements transaction propagation via gossipsub, peer management,
//! rate limiting, and state sync (per spec §8, §13.5.4).

pub mod gossip;
pub mod limits;
pub mod p2p;

pub use gossip::*;
pub use limits::*;
pub use p2p::{CommonwareNetwork, CommonwareConfig, load_or_generate_identity_key, Network, NetworkMessage, BlockAnnouncement, TransactionMessage, SyncRequest, SyncResponse, OraclePriceRequest, OraclePriceSubmission, UpgradeAnnouncement, InMemoryNetwork};

//! Network trait abstraction over commonware-p2p.

use crate::limits::NetworkError;

/// Abstract network interface.
///
/// This trait defines the contract for P2P communication.
/// At runtime, `CommonwareNetwork` provides a real commonware-p2p implementation.
#[async_trait::async_trait]
pub trait Network: Send + Sync + 'static {
    /// Send a message to all connected peers
    async fn broadcast(&self, channel: u64, message: Vec<u8>);

    /// Try to broadcast a message, returning an error if the send fails
    /// instead of silently dropping it.
    async fn try_broadcast(&self, channel: u64, message: Vec<u8>) -> Result<(), NetworkError>;

    /// Send a message to specific peers on the given channel.
    ///
    /// Like `broadcast`, the channel byte is prepended to the payload so the
    /// receiving node's dispatch loop (`decode_with_channel`) routes the
    /// message to the correct handler. Forgetting the channel here used to
    /// silently drop SyncRequest/SyncResponse traffic on the floor.
    async fn send_to(&self, channel: u64, peers: Vec<String>, message: Vec<u8>);

    /// Receive messages from the network
    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError>;

    /// Get the number of connected peers
    fn peer_count(&self) -> usize;

    /// Get the list of connected peer IDs
    fn peer_ids(&self) -> Vec<String>;

    /// Connect to a peer
    async fn connect(&self, address: &str) -> Result<(), NetworkError>;

    /// Disconnect from a peer
    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError>;

    /// Check if the network is healthy
    fn is_healthy(&self) -> bool;
}

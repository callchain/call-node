//! In-memory network implementation for local testing.

use crate::limits::NetworkError;
use crate::p2p::trait_::Network;

/// In-memory network implementation for local testing.
///
/// Simulates a P2P network with in-memory message buffering.
/// Useful for testing without actual network connectivity.
pub struct InMemoryNetwork {
    connected_peers: std::sync::Mutex<Vec<String>>,
    message_buffer: std::sync::Mutex<Vec<(String, u64, Vec<u8>)>>,
}

impl InMemoryNetwork {
    pub fn new() -> Self {
        Self {
            connected_peers: std::sync::Mutex::new(Vec::new()),
            message_buffer: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Simulate receiving a message (pushes to buffer for testing)
    pub fn simulate_receive(&self, peer_id: String, channel: u64, message: Vec<u8>) {
        if let Ok(mut buffer) = self.message_buffer.lock() {
            buffer.push((peer_id, channel, message));
        }
    }
}

impl Default for InMemoryNetwork {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Network for InMemoryNetwork {
    async fn broadcast(&self, channel: u64, message: Vec<u8>) {
        let _ = self.try_broadcast(channel, message).await;
    }

    async fn try_broadcast(&self, channel: u64, message: Vec<u8>) -> Result<(), NetworkError> {
        let mut buffer = self.message_buffer.lock().map_err(|_| NetworkError::NetworkError("lock poisoned".into()))?;
        buffer.push(("broadcast".into(), channel, message));
        Ok(())
    }

    async fn send_to(&self, channel: u64, peers: Vec<String>, message: Vec<u8>) {
        if let Ok(mut buffer) = self.message_buffer.lock() {
            for peer in &peers {
                buffer.push((peer.clone(), channel, message.clone()));
            }
        }
    }

    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError> {
        let mut buffer = self.message_buffer.lock().map_err(|_| NetworkError::NetworkError("lock poisoned".into()))?;
        buffer.pop().ok_or_else(|| NetworkError::NetworkError("no messages".into()))
    }

    fn peer_count(&self) -> usize {
        self.connected_peers.lock().map(|p| p.len()).unwrap_or(0)
    }

    fn peer_ids(&self) -> Vec<String> {
        self.connected_peers.lock().map(|p| p.clone()).unwrap_or_default()
    }

    async fn connect(&self, address: &str) -> Result<(), NetworkError> {
        if let Ok(mut peers) = self.connected_peers.lock() {
            if !peers.contains(&address.to_string()) {
                peers.push(address.to_string());
            }
        }
        Ok(())
    }

    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError> {
        if let Ok(mut peers) = self.connected_peers.lock() {
            peers.retain(|p| p != peer_id);
        }
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.connected_peers.lock().map(|p| !p.is_empty()).unwrap_or(false)
    }
}

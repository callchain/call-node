//! T7.1 — Network Limits (per spec §13.5.4)
//!
//! NetworkLimits struct with default values for P2P attack protection.

use serde::{Deserialize, Serialize};

/// Network-level rate limiting and connection limits (per spec §13.5.4)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NetworkLimits {
    /// Maximum peer connections (default 50)
    pub max_peers: u32,
    /// Maximum messages per second per connection (default 100)
    pub max_messages_per_second: u32,
    /// Maximum message size in bytes (default 10 MB)
    pub max_message_size: u32,
    /// Known transaction deduplication cache size (default 1,000,000)
    pub known_txs_cache_size: u32,
    /// Ban duration for malicious peers in seconds (default 3600 = 1 hour)
    pub ban_duration_seconds: u64,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            max_peers: 50,
            max_messages_per_second: 100,
            max_message_size: 10 * 1024 * 1024, // 10 MB
            known_txs_cache_size: 1_000_000,
            ban_duration_seconds: 3600, // 1 hour
        }
    }
}

impl NetworkLimits {
    /// Create with custom values
    pub const fn new(
        max_peers: u32,
        max_messages_per_second: u32,
        max_message_size: u32,
        known_txs_cache_size: u32,
        ban_duration_seconds: u64,
    ) -> Self {
        Self {
            max_peers,
            max_messages_per_second,
            max_message_size,
            known_txs_cache_size,
            ban_duration_seconds,
        }
    }

    /// Validate that limits are within reasonable bounds
    pub fn validate(&self) -> Result<(), NetworkError> {
        if self.max_peers == 0 {
            return Err(NetworkError::InvalidLimits("max_peers must be > 0"));
        }
        if self.max_messages_per_second == 0 {
            return Err(NetworkError::InvalidLimits(
                "max_messages_per_second must be > 0",
            ));
        }
        if self.max_message_size == 0 {
            return Err(NetworkError::InvalidLimits(
                "max_message_size must be > 0",
            ));
        }
        if self.known_txs_cache_size == 0 {
            return Err(NetworkError::InvalidLimits(
                "known_txs_cache_size must be > 0",
            ));
        }
        if self.ban_duration_seconds == 0 {
            return Err(NetworkError::InvalidLimits(
                "ban_duration_seconds must be > 0",
            ));
        }
        // Cap at reasonable maximums to prevent misconfiguration
        if self.max_peers > 10_000 {
            return Err(NetworkError::InvalidLimits(
                "max_peers exceeds reasonable limit (10,000)",
            ));
        }
        if self.max_message_size > 100 * 1024 * 1024 {
            return Err(NetworkError::InvalidLimits(
                "max_message_size exceeds reasonable limit (100 MB)",
            ));
        }
        Ok(())
    }
}

/// Network error types
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("invalid limits config: {0}")]
    InvalidLimits(&'static str),
    #[error("message too large: {size} bytes (max {max})")]
    MessageTooLarge { size: usize, max: usize },
    #[error("rate limit exceeded for peer")]
    RateLimitExceeded,
    #[error("peer limit reached: {current}/{max}")]
    PeerLimitReached { current: usize, max: usize },
    #[error("peer banned: {reason}")]
    PeerBanned { reason: String },
    #[error("peer not found: {peer_id}")]
    PeerNotFound { peer_id: String },
    #[error("network error: {0}")]
    NetworkError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_limits_defaults() {
        let limits = NetworkLimits::default();
        assert_eq!(limits.max_peers, 50);
        assert_eq!(limits.max_messages_per_second, 100);
        assert_eq!(limits.max_message_size, 10 * 1024 * 1024);
        assert_eq!(limits.known_txs_cache_size, 1_000_000);
        assert_eq!(limits.ban_duration_seconds, 3600);
    }

    #[test]
    fn test_network_limits_custom() {
        let limits = NetworkLimits::new(100, 200, 5 * 1024 * 1024, 500_000, 7200);
        assert_eq!(limits.max_peers, 100);
        assert_eq!(limits.max_messages_per_second, 200);
        assert_eq!(limits.max_message_size, 5 * 1024 * 1024);
        assert_eq!(limits.known_txs_cache_size, 500_000);
        assert_eq!(limits.ban_duration_seconds, 7200);
    }

    #[test]
    fn test_network_limits_validate_ok() {
        let limits = NetworkLimits::default();
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn test_network_limits_validate_zero_peers() {
        let limits = NetworkLimits::new(0, 100, 1024, 1000, 3600);
        assert!(limits.validate().is_err());
    }

    #[test]
    fn test_network_limits_validate_excessive_peers() {
        let limits = NetworkLimits::new(20_000, 100, 1024, 1000, 3600);
        assert!(limits.validate().is_err());
    }

    #[test]
    fn test_network_limits_validate_excessive_message_size() {
        let limits = NetworkLimits::new(50, 100, 200 * 1024 * 1024, 1000, 3600);
        assert!(limits.validate().is_err());
    }

    #[test]
    fn test_network_error_display() {
        let err = NetworkError::MessageTooLarge {
            size: 15_000_000,
            max: 10_485_760,
        };
        let msg = err.to_string();
        assert!(msg.contains("15000000"));
        assert!(msg.contains("10485760"));
    }
}

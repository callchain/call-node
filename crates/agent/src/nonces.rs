//! Agent nonce tracking (per spec §6.6)

use call_primitives::Address;
use crate::AgentError;

/// Agent nonces: keyed by (owner, agent_id)
///
/// Per spec §6.6: agent transactions have their own nonce sequence
/// to prevent replay attacks.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentNonces {
    /// (owner, agent_id) -> nonce
    nonces: std::collections::HashMap<(Address, u64), u64>,
}

impl AgentNonces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get current nonce for an agent
    pub fn get_nonce(&self, owner: Address, agent_id: u64) -> u64 {
        self.nonces.get(&(owner, agent_id)).copied().unwrap_or(0)
    }

    /// Check and increment nonce (returns error if stale/duplicate)
    pub fn check_and_increment(
        &mut self,
        owner: Address,
        agent_id: u64,
        expected_nonce: u64,
    ) -> Result<(), AgentError> {
        let key = (owner, agent_id);
        let current = self.nonces.get(&key).copied().unwrap_or(0);

        if expected_nonce != current {
            return Err(AgentError::AgentNonceError(format!(
                "expected nonce {}, got {}",
                current, expected_nonce
            )));
        }

        self.nonces.insert(key, current + 1);
        Ok(())
    }

    /// Force set nonce (for recovery or admin operations)
    pub fn set_nonce(&mut self, owner: Address, agent_id: u64, nonce: u64) {
        self.nonces.insert((owner, agent_id), nonce);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;

    #[test]
    fn test_agent_nonce_tracking() {
        let mut nonces = AgentNonces::new();
        let owner = test_addr(1);

        assert_eq!(nonces.get_nonce(owner, 0), 0);
        assert!(nonces.check_and_increment(owner, 0, 0).is_ok());
        assert_eq!(nonces.get_nonce(owner, 0), 1);
        assert!(nonces.check_and_increment(owner, 0, 1).is_ok());
        assert_eq!(nonces.get_nonce(owner, 0), 2);

        // Stale nonce
        assert!(nonces.check_and_increment(owner, 0, 0).is_err());
        // Duplicate nonce
        assert!(nonces.check_and_increment(owner, 0, 1).is_err());
    }
}

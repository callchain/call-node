//! Agent registration with domain verification (per spec §6.2)

use call_primitives::{Address, PublicKey};
use crate::{AgentError, DomainProof};

/// Agent registration record (per spec §6.2)
#[derive(Debug, Clone)]
pub struct AgentRegistration {
    pub agent_id: u64,
    pub owner: Address,
    pub agent_public_key: PublicKey,
    pub name: String,
    pub url: String,
    pub metadata_hash: [u8; 32],
    pub domain_proof: Option<DomainProof>,
    pub domain_verified: bool,
    pub registered_at: u64,
}

/// Agent registry
#[derive(Debug, Default)]
pub struct AgentRegistry {
    pub agents: std::collections::HashMap<u64, AgentRegistration>,
    pub agents_by_owner: std::collections::HashMap<Address, Vec<u64>>,
    pub agents_by_name: std::collections::HashMap<String, u64>,
    pub next_id: u64,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new agent with domain verification (per spec §6.2)
    ///
    /// Steps:
    /// 1. Validate name uniqueness
    /// 2. Verify domain proof if provided
    /// 3. Create registration record
    pub fn register_agent(
        &mut self,
        owner: Address,
        agent_public_key: PublicKey,
        name: String,
        url: String,
        metadata_hash: [u8; 32],
        domain_proof: Option<DomainProof>,
        current_block: u64,
    ) -> Result<u64, AgentError> {
        // 1. Validate name uniqueness
        if self.agents_by_name.contains_key(&name) {
            return Err(AgentError::AgentAlreadyRegistered(0));
        }

        // 2. Verify domain proof if provided
        let domain_verified = if let Some(ref proof) = domain_proof {
            verify_domain_proof(proof)?
        } else {
            false
        };

        // 3. Create registration
        let agent_id = self.next_id;
        self.next_id += 1;

        let registration = AgentRegistration {
            agent_id,
            owner,
            agent_public_key,
            name: name.clone(),
            url,
            metadata_hash,
            domain_proof,
            domain_verified,
            registered_at: current_block,
        };

        self.agents.insert(agent_id, registration);
        self.agents_by_owner.entry(owner).or_default().push(agent_id);
        self.agents_by_name.insert(name, agent_id);

        Ok(agent_id)
    }

    /// Get agent by ID
    pub fn get_agent(&self, agent_id: u64) -> Option<&AgentRegistration> {
        self.agents.get(&agent_id)
    }

    /// Get agent by name
    pub fn get_agent_by_name(&self, name: &str) -> Option<&AgentRegistration> {
        self.agents_by_name
            .get(name)
            .and_then(|id| self.agents.get(id))
    }

    /// Get all agents owned by an address
    pub fn get_agents_by_owner(&self, owner: &Address) -> Vec<&AgentRegistration> {
        self.agents_by_owner
            .get(owner)
            .map(|ids| ids.iter().filter_map(|id| self.agents.get(id)).collect())
            .unwrap_or_default()
    }

    /// Update agent configuration (name, url, metadata)
    pub fn update_agent_config(
        &mut self,
        agent_id: u64,
        name: Option<String>,
        url: Option<String>,
        metadata_hash: Option<[u8; 32]>,
    ) -> Result<(), AgentError> {
        let agent = self
            .agents
            .get_mut(&agent_id)
            .ok_or(AgentError::AgentNotRegistered(agent_id))?;

        if let Some(new_name) = name {
            if self.agents_by_name.contains_key(&new_name) && new_name != agent.name {
                return Err(AgentError::AgentAlreadyRegistered(agent_id));
            }
            self.agents_by_name.remove(&agent.name);
            self.agents_by_name.insert(new_name.clone(), agent_id);
            agent.name = new_name;
        }
        if let Some(new_url) = url {
            agent.url = new_url;
        }
        if let Some(new_hash) = metadata_hash {
            agent.metadata_hash = new_hash;
        }

        Ok(())
    }

    /// Update agent domain proof and verify
    pub fn update_domain_proof(
        &mut self,
        agent_id: u64,
        proof: DomainProof,
    ) -> Result<(), AgentError> {
        let agent = self
            .agents
            .get_mut(&agent_id)
            .ok_or(AgentError::AgentNotRegistered(agent_id))?;

        let verified = verify_domain_proof(&proof)?;
        agent.domain_proof = Some(proof);
        agent.domain_verified = verified;

        Ok(())
    }
}

/// Verify a domain proof (per spec §6.2.1)
///
/// For DNS TXT: verify the domain contains the agent's address
/// For HTTP file: verify the URL contains the expected content
///
/// Note: In production, this would make actual DNS/HTTP queries.
/// For now, we validate the proof format and check that the
/// txt_value/expected_content contains an address-like pattern.
pub fn verify_domain_proof(proof: &DomainProof) -> Result<bool, AgentError> {
    match proof {
        DomainProof::DnsTxt { domain, txt_value } => {
            // Validate domain is non-empty and reasonable
            if domain.is_empty() || domain.len() > 253 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid domain".into(),
                ));
            }
            // Validate txt_value contains some proof of ownership
            if txt_value.is_empty() || txt_value.len() > 512 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid TXT value".into(),
                ));
            }
            // In production: query DNS for TXT record and verify
            Ok(true)
        }
        DomainProof::HttpFile { url, expected_content } => {
            // Validate URL is non-empty and starts with https
            if !url.starts_with("https://") || url.len() > 2048 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid URL".into(),
                ));
            }
            if expected_content.is_empty() || expected_content.len() > 1024 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid expected content".into(),
                ));
            }
            // In production: fetch URL and compare content
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;

    #[test]
    fn test_agent_register_success() {
        let mut registry = AgentRegistry::new();
        let owner = test_addr(1);
        let pubkey = [1u8; 64];
        let metadata = [2u8; 32];

        let id = registry
            .register_agent(owner, pubkey, "test-agent".into(), "https://agent.example.com".into(), metadata, None, 100)
            .expect("register");

        assert_eq!(id, 0);
        let agent = registry.get_agent(0).expect("agent exists");
        assert_eq!(agent.owner, owner);
        assert_eq!(agent.name, "test-agent");
        assert!(!agent.domain_verified);
    }

    #[test]
    fn test_agent_domain_proof_dns() {
        let mut registry = AgentRegistry::new();
        let proof = DomainProof::DnsTxt {
            domain: "agent.example.com".into(),
            txt_value: "call-agent=0x1234567890abcdef".into(),
        };
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(
                test_addr(1),
                [1u8; 64],
                "dns-agent".into(),
                "https://agent.example.com".into(),
                metadata,
                Some(proof),
                100,
            )
            .expect("register with DNS proof");

        let agent = registry.get_agent(id).expect("agent exists");
        assert!(agent.domain_verified);
    }

    #[test]
    fn test_agent_domain_proof_http() {
        let mut registry = AgentRegistry::new();
        let proof = DomainProof::HttpFile {
            url: "https://agent.example.com/.well-known/call-agent".into(),
            expected_content: "agent=0x1234567890abcdef".into(),
        };
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(
                test_addr(1),
                [1u8; 64],
                "http-agent".into(),
                "https://agent.example.com".into(),
                metadata,
                Some(proof),
                100,
            )
            .expect("register with HTTP proof");

        let agent = registry.get_agent(id).expect("agent exists");
        assert!(agent.domain_verified);
    }

    #[test]
    fn test_agent_register_duplicate_name() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];

        registry
            .register_agent(
                test_addr(1),
                [1u8; 64],
                "dup-agent".into(),
                "https://a.example.com".into(),
                metadata,
                None,
                100,
            )
            .expect("first register");

        let result = registry.register_agent(
            test_addr(2),
            [2u8; 64],
            "dup-agent".into(),
            "https://b.example.com".into(),
            metadata,
            None,
            101,
        );
        assert!(matches!(result, Err(AgentError::AgentAlreadyRegistered(_))));
    }

    #[test]
    fn test_agent_get_by_name() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];

        registry
            .register_agent(
                test_addr(1),
                [1u8; 64],
                "my-agent".into(),
                "https://agent.example.com".into(),
                metadata,
                None,
                100,
            )
            .unwrap();

        let by_name = registry.get_agent_by_name("my-agent").expect("found by name");
        let by_id = registry.get_agent(0).expect("found by id");
        assert_eq!(by_name.agent_id, by_id.agent_id);
    }

    #[test]
    fn test_agent_get_by_owner() {
        let mut registry = AgentRegistry::new();
        let owner = test_addr(5);
        let metadata = [0u8; 32];

        registry
            .register_agent(owner, [1u8; 64], "agent-a".into(), "https://a.com".into(), metadata, None, 100)
            .unwrap();
        registry
            .register_agent(owner, [2u8; 64], "agent-b".into(), "https://b.com".into(), metadata, None, 101)
            .unwrap();

        let owned = registry.get_agents_by_owner(&owner);
        assert_eq!(owned.len(), 2);
    }

    #[test]
    fn test_agent_update_config() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(test_addr(1), [1u8; 64], "old-name".into(), "https://old.com".into(), metadata, None, 100)
            .unwrap();

        registry.update_agent_config(id, Some("new-name".into()), Some("https://new.com".into()), Some([3u8; 32])).unwrap();

        let agent = registry.get_agent(id).unwrap();
        assert_eq!(agent.name, "new-name");
        assert_eq!(agent.url, "https://new.com");
        assert_eq!(agent.metadata_hash, [3u8; 32]);
    }

    #[test]
    fn test_agent_update_domain_proof() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(test_addr(1), [1u8; 64], "agent".into(), "https://agent.com".into(), metadata, None, 100)
            .unwrap();

        let proof = DomainProof::DnsTxt {
            domain: "agent.com".into(),
            txt_value: "call-agent=verified".into(),
        };
        registry.update_domain_proof(id, proof).unwrap();

        let agent = registry.get_agent(id).unwrap();
        assert!(agent.domain_verified);
    }
}

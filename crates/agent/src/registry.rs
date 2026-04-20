//! Agent registration with domain verification (per spec §6.2)

use call_primitives::{Address, PublicKey};
use crate::{AgentError, DomainProof, AgentPermissions};

/// Agent registration record (per spec §6.2)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentRegistration {
    pub agent_id: u64,
    pub owner: Address,
    #[serde(with = "serde_bytes")]
    pub agent_public_key: call_primitives::PublicKey,
    pub name: String,
    pub url: String,
    pub metadata_hash: [u8; 32],
    pub domain_proof: Option<DomainProof>,
    pub domain_verified: bool,
    pub registered_at: u64,
    /// Agent permissions (controls what the agent can do)
    #[serde(default)]
    pub permissions: AgentPermissions,
}

/// Agent registry
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AgentRegistry {
    pub agents: std::collections::HashMap<u64, AgentRegistration>,
    pub agents_by_owner: std::collections::HashMap<Address, Vec<u64>>,
    pub agents_by_name: std::collections::HashMap<String, u64>,
    pub next_id: u64,
    /// Registration fee (in fee_asset_id units). 0 = free registration.
    pub registration_fee: u128,
    /// Asset ID for registration fee (default: 1 = CALL).
    pub fee_asset_id: call_primitives::AssetId,
    #[serde(skip)]
    domain_verifier: Option<Box<dyn DomainVerifier>>,
}

impl std::fmt::Debug for AgentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRegistry")
            .field("agents", &self.agents)
            .field("agents_by_owner", &self.agents_by_owner)
            .field("agents_by_name", &self.agents_by_name)
            .field("next_id", &self.next_id)
            .field("registration_fee", &self.registration_fee)
            .field("fee_asset_id", &self.fee_asset_id)
            .field("domain_verifier", &self.domain_verifier.is_some())
            .finish()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self {
            agents: Default::default(),
            agents_by_owner: Default::default(),
            agents_by_name: Default::default(),
            next_id: Default::default(),
            registration_fee: 0,
            fee_asset_id: 1,
            domain_verifier: None,
        }
    }
}

impl AgentRegistry {
    /// Create a new registry with real domain verification by default.
    pub fn new() -> Self {
        Self {
            agents: Default::default(),
            agents_by_owner: Default::default(),
            agents_by_name: Default::default(),
            next_id: Default::default(),
            registration_fee: 0,
            fee_asset_id: 1,
            domain_verifier: Some(Box::new(RealDomainVerifier)),
        }
    }

    /// Create a registry with format-only domain verification (for testing).
    pub fn new_with_format_verifier() -> Self {
        Self::default()
    }

    /// Set the registration fee (in fee_asset_id units).
    pub fn with_registration_fee(mut self, fee: u128, asset_id: call_primitives::AssetId) -> Self {
        self.registration_fee = fee;
        self.fee_asset_id = asset_id;
        self
    }

    /// Set a custom domain verifier for this registry.
    pub fn with_verifier(mut self, verifier: Box<dyn DomainVerifier>) -> Self {
        self.domain_verifier = Some(verifier);
        self
    }

    fn verify_proof(&self, proof: &DomainProof) -> Result<bool, AgentError> {
        match &self.domain_verifier {
            Some(verifier) => verifier.verify(proof),
            None => verify_domain_proof(proof),
        }
    }

    /// Register a new agent with domain verification and optional fee (per spec §6.2)
    ///
    /// Steps:
    /// 1. Validate name uniqueness
    /// 2. Deduct registration fee if set
    /// 3. Verify domain proof if provided
    /// 4. Create registration record
    pub fn register_agent(
        &mut self,
        owner: Address,
        agent_public_key: PublicKey,
        name: String,
        url: String,
        metadata_hash: [u8; 32],
        domain_proof: Option<DomainProof>,
        current_block: u64,
        balances: Option<&mut call_protocol::balances::BalanceState>,
    ) -> Result<u64, AgentError> {
        // 1. Validate name uniqueness
        if self.agents_by_name.contains_key(&name) {
            return Err(AgentError::AgentAlreadyRegistered(0));
        }

        // 2. Deduct registration fee if configured
        if self.registration_fee > 0 {
            if let Some(balances) = balances {
                balances
                    .deduct_balance(self.fee_asset_id, owner, self.registration_fee)
                    .map_err(|_| AgentError::InsufficientBalanceForRegistration(self.registration_fee))?;
            } else {
                return Err(AgentError::InsufficientBalanceForRegistration(self.registration_fee));
            }
        }

        // 3. Verify domain proof if provided
        let domain_verified = if let Some(ref proof) = domain_proof {
            self.verify_proof(proof)?
        } else {
            false
        };

        // 4. Create registration
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
            permissions: AgentPermissions::default(),
        };

        self.agents.insert(agent_id, registration);
        self.agents_by_owner.entry(owner).or_default().push(agent_id);
        self.agents_by_name.insert(name, agent_id);

        Ok(agent_id)
    }

    /// Unregister an agent (removes from all indexes).
    /// Returns the removed registration if found.
    pub fn unregister_agent(&mut self, agent_id: u64) -> Option<AgentRegistration> {
        let reg = self.agents.remove(&agent_id)?;
        self.agents_by_name.remove(&reg.name);
        if let Some(list) = self.agents_by_owner.get_mut(&reg.owner) {
            list.retain(|&id| id != agent_id);
            if list.is_empty() {
                self.agents_by_owner.remove(&reg.owner);
            }
        }
        Some(reg)
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
        let verified = self.verify_proof(&proof)?;

        let agent = self
            .agents
            .get_mut(&agent_id)
            .ok_or(AgentError::AgentNotRegistered(agent_id))?;

        agent.domain_proof = Some(proof);
        agent.domain_verified = verified;

        Ok(())
    }
}

/// Domain verifier trait for verifying agent domain proofs.
/// Implement this to provide actual DNS/HTTP verification.
pub trait DomainVerifier: Send + Sync {
    fn verify(&self, proof: &DomainProof) -> Result<bool, AgentError>;
}

/// Default domain verifier that validates proof format but does not
/// make network requests. Useful for testing and internal validation.
pub struct DefaultDomainVerifier;

impl DomainVerifier for DefaultDomainVerifier {
    fn verify(&self, proof: &DomainProof) -> Result<bool, AgentError> {
        verify_domain_proof_format(proof)
    }
}

/// Real domain verifier that performs actual DNS TXT and HTTP file lookups.
pub struct RealDomainVerifier;

impl DomainVerifier for RealDomainVerifier {
    fn verify(&self, proof: &DomainProof) -> Result<bool, AgentError> {
        verify_domain_proof_network(proof)
    }
}

/// Verify a domain proof (per spec §6.2.1)
///
/// For DNS TXT: verify the domain contains the agent's address
/// For HTTP file: verify the URL contains the expected content
///
/// This function validates the proof format. For actual DNS/HTTP
/// queries, use `RealDomainVerifier`.
pub fn verify_domain_proof(proof: &DomainProof) -> Result<bool, AgentError> {
    verify_domain_proof_format(proof)
}

fn verify_domain_proof_format(proof: &DomainProof) -> Result<bool, AgentError> {
    match proof {
        DomainProof::DnsTxt { domain, txt_value } => {
            if domain.is_empty() || domain.len() > 253 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid domain".into(),
                ));
            }
            if txt_value.is_empty() || txt_value.len() > 512 {
                return Err(AgentError::DomainVerificationFailed(
                    "invalid TXT value".into(),
                ));
            }
            Ok(true)
        }
        DomainProof::HttpFile { url, expected_content } => {
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
            Ok(true)
        }
    }
}

/// Perform real DNS TXT and HTTP verification against live network.
fn verify_domain_proof_network(proof: &DomainProof) -> Result<bool, AgentError> {
    match proof {
        DomainProof::DnsTxt { domain, txt_value } => {
            verify_domain_proof_format(proof)?;

            let resolver = hickory_resolver::Resolver::from_system_conf()
                .map_err(|e| AgentError::DomainVerificationFailed(format!("dns resolver init failed: {e}")))?;

            let lookup = resolver.txt_lookup(domain.as_str())
                .map_err(|e| AgentError::DomainVerificationFailed(format!("dns lookup failed: {e}")))?;

            for record in lookup {
                let data: String = record.iter()
                    .map(|b| String::from_utf8_lossy(b))
                    .collect();
                if data.contains(txt_value) {
                    return Ok(true);
                }
            }

            Ok(false)
        }
        DomainProof::HttpFile { url, expected_content } => {
            verify_domain_proof_format(proof)?;

            let body = ureq::get(url)
                .call()
                .map_err(|e| AgentError::DomainVerificationFailed(format!("http fetch failed: {e}")))?
                .into_string()
                .map_err(|e| AgentError::DomainVerificationFailed(format!("http read failed: {e}")))?;

            Ok(body.contains(expected_content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;

    #[test]
    fn test_agent_register_success() {
        let mut registry = AgentRegistry::new_with_format_verifier();
        let owner = test_addr(1);
        let pubkey = [1u8; 64];
        let metadata = [2u8; 32];

        let id = registry
            .register_agent(owner, pubkey, "test-agent".into(), "https://agent.example.com".into(), metadata, None, 100,
                None)
            .expect("register");

        assert_eq!(id, 0);
        let agent = registry.get_agent(0).expect("agent exists");
        assert_eq!(agent.owner, owner);
        assert_eq!(agent.name, "test-agent");
        assert!(!agent.domain_verified);
    }

    #[test]
    fn test_agent_domain_proof_dns() {
        let mut registry = AgentRegistry::new_with_format_verifier();
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
                None
            )
            .expect("register with DNS proof");

        let agent = registry.get_agent(id).expect("agent exists");
        assert!(agent.domain_verified);
    }

    #[test]
    fn test_agent_domain_proof_http() {
        let mut registry = AgentRegistry::new_with_format_verifier();
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
                None
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
                None
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
            None,
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
                None
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
            .register_agent(owner, [1u8; 64], "agent-a".into(), "https://a.com".into(), metadata, None, 100,
                None)
            .unwrap();
        registry
            .register_agent(owner, [2u8; 64], "agent-b".into(), "https://b.com".into(), metadata, None, 101,
                None)
            .unwrap();

        let owned = registry.get_agents_by_owner(&owner);
        assert_eq!(owned.len(), 2);
    }

    #[test]
    fn test_agent_update_config() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(test_addr(1), [1u8; 64], "old-name".into(), "https://old.com".into(), metadata, None, 100,
                None)
            .unwrap();

        registry.update_agent_config(id, Some("new-name".into()), Some("https://new.com".into()), Some([3u8; 32])).unwrap();

        let agent = registry.get_agent(id).unwrap();
        assert_eq!(agent.name, "new-name");
        assert_eq!(agent.url, "https://new.com");
        assert_eq!(agent.metadata_hash, [3u8; 32]);
    }

    #[test]
    fn test_agent_update_domain_proof() {
        let mut registry = AgentRegistry::new_with_format_verifier();
        let metadata = [0u8; 32];

        let id = registry
            .register_agent(test_addr(1), [1u8; 64], "agent".into(), "https://agent.com".into(), metadata, None, 100,
                None)
            .unwrap();

        let proof = DomainProof::DnsTxt {
            domain: "agent.com".into(),
            txt_value: "call-agent=verified".into(),
        };
        registry.update_domain_proof(id, proof).unwrap();

        let agent = registry.get_agent(id).unwrap();
        assert!(agent.domain_verified);
    }

    #[test]
    fn test_agent_register_with_fee_success() {
        let mut registry = AgentRegistry::new_with_format_verifier()
            .with_registration_fee(500, 1);
        let mut balances = call_protocol::balances::BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 1000).unwrap();

        let id = registry
            .register_agent(
                test_addr(1), [1u8; 64], "fee-agent".into(), "https://fee.com".into(), [0u8; 32], None, 100,
                Some(&mut balances),
            )
            .unwrap();

        assert_eq!(id, 0);
        assert_eq!(balances.get_balance(1, &test_addr(1)), 500);
    }

    #[test]
    fn test_agent_register_with_fee_insufficient_balance() {
        let mut registry = AgentRegistry::new_with_format_verifier()
            .with_registration_fee(500, 1);
        let mut balances = call_protocol::balances::BalanceState::new();
        balances.balances.set_balance(1, test_addr(1), 100).unwrap();

        let result = registry.register_agent(
            test_addr(1), [1u8; 64], "fee-agent".into(), "https://fee.com".into(), [0u8; 32], None, 100,
            Some(&mut balances),
        );

        assert!(matches!(result, Err(AgentError::InsufficientBalanceForRegistration(500))));
        // Balance unchanged
        assert_eq!(balances.get_balance(1, &test_addr(1)), 100);
    }

    #[test]
    fn test_agent_register_with_fee_no_balances() {
        let mut registry = AgentRegistry::new_with_format_verifier()
            .with_registration_fee(500, 1);

        let result = registry.register_agent(
            test_addr(1), [1u8; 64], "fee-agent".into(), "https://fee.com".into(), [0u8; 32], None, 100,
            None,
        );

        assert!(matches!(result, Err(AgentError::InsufficientBalanceForRegistration(500))));
    }
}

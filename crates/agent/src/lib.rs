//! Callchain Agent Payments (per spec §6)
//!
//! - Agent registration with domain verification
//! - Agent permissions and fee configuration
//! - Agent balance management
//! - Agent transaction verification and execution with gas discount

mod registry;
mod permissions;
mod balances;
mod executor;

pub use registry::*;
pub use permissions::*;
pub use balances::*;
pub use executor::*;

use call_primitives::{Address, AssetId, Signature};
use call_protocol::ProtocolTransaction;
use thiserror::Error;

/// Domain proof for agent registration (per spec §6.2)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DomainProof {
    DnsTxt {
        domain: String,
        txt_value: String,
    },
    HttpFile {
        url: String,
        expected_content: String,
    },
}

/// Fee payer mode for agent transactions (per spec §6.5)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FeePayer {
    SelfPay,
    OwnerPays,
    ThirdParty { payer: Address },
}

/// Agent fee configuration (per spec §6.5)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentFeeConfig {
    pub fee_payer: FeePayer,
    pub owner_max_daily_fee: u128,
    pub owner_max_total_fee: u128,
    pub require_owner_signature_above: u128,
}

impl Default for AgentFeeConfig {
    fn default() -> Self {
        Self {
            fee_payer: FeePayer::SelfPay,
            owner_max_daily_fee: u128::MAX,
            owner_max_total_fee: u128::MAX,
            require_owner_signature_above: u128::MAX,
        }
    }
}

/// Agent funding action (per spec §6.7)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentFundingAction {
    Grant {
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
    },
    TopUp {
        agent_id: u64,
        asset_id: AssetId,
        amount: u128,
    },
    Revoke {
        agent_id: u64,
        asset_id: AssetId,
    },
    UpdateConfig {
        agent_id: u64,
        new_config: AgentFeeConfig,
    },
}

/// Signed agent transaction (per spec §6.6)
#[derive(Debug, Clone)]
pub struct SignedAgentTx {
    pub protocol_tx: ProtocolTransaction,
    pub owner_signature: Option<Signature>,
}

/// Agent error
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent not registered: {0}")]
    AgentNotRegistered(u64),
    #[error("agent already registered: {0}")]
    AgentAlreadyRegistered(u64),
    #[error("domain verification failed: {0}")]
    DomainVerificationFailed(String),
    #[error("agent signature verification failed")]
    InvalidAgentSignature,
    #[error("agent nonce stale or duplicate: {0}")]
    AgentNonceError(String),
    #[error("agent permission denied: {0}")]
    PermissionDenied(String),
    #[error("agent transaction expired")]
    TransactionExpired,
    #[error("owner signature required but not provided")]
    OwnerSignatureRequired,
    #[error("owner signature verification failed")]
    InvalidOwnerSignature,
    #[error("agent fee limit exceeded: daily={daily}, total={total}")]
    FeeLimitExceeded { daily: u128, total: u128 },
    #[error("agent balance insufficient: agent={0}, need={1}")]
    InsufficientAgentBalance(u64, u128),
    #[error("invalid funding action: {0}")]
    InvalidFundingAction(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

#[cfg(test)]
pub mod test_utils {
    use call_primitives::Address;
    pub fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_agent_fee_config_defaults() {
        let config = AgentFeeConfig::default();
        assert_eq!(config.fee_payer, FeePayer::SelfPay);
        assert_eq!(config.owner_max_daily_fee, u128::MAX);
        assert_eq!(config.owner_max_total_fee, u128::MAX);
        assert_eq!(config.require_owner_signature_above, u128::MAX);
    }

    #[test]
    fn test_domain_proof_dns() {
        let proof = DomainProof::DnsTxt {
            domain: "example.com".into(),
            txt_value: "call-agent=0x123".into(),
        };
        match proof {
            DomainProof::DnsTxt { domain, txt_value } => {
                assert_eq!(domain, "example.com");
                assert_eq!(txt_value, "call-agent=0x123");
            }
            DomainProof::HttpFile { .. } => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_domain_proof_http() {
        let proof = DomainProof::HttpFile {
            url: "https://example.com/.well-known/call-agent".into(),
            expected_content: "agent=0x123".into(),
        };
        match proof {
            DomainProof::HttpFile { url, expected_content } => {
                assert_eq!(url, "https://example.com/.well-known/call-agent");
                assert_eq!(expected_content, "agent=0x123");
            }
            DomainProof::DnsTxt { .. } => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_funding_action_variants() {
        let grant = AgentFundingAction::Grant {
            agent_id: 1,
            asset_id: 1,
            amount: 1000,
        };
        assert_eq!(grant, AgentFundingAction::Grant {
            agent_id: 1, asset_id: 1, amount: 1000
        });

        let revoke = AgentFundingAction::Revoke {
            agent_id: 1,
            asset_id: 1,
        };
        match revoke {
            AgentFundingAction::Revoke { agent_id, asset_id } => {
                assert_eq!(agent_id, 1);
                assert_eq!(asset_id, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_error_types() {
        assert!(matches!(
            AgentError::AgentNotRegistered(1),
            AgentError::AgentNotRegistered(1)
        ));
        assert!(matches!(
            AgentError::PermissionDenied("asset not allowed".into()),
            AgentError::PermissionDenied(_)
        ));
    }
}

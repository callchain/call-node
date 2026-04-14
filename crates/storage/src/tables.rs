//! Table definitions for the Callchain storage layer.
//!
//! Maps the logical directory structure from spec §10.2 to table definitions.
//! Each struct represents a table with a unique NAME constant for registration.

use call_primitives::{Address, Balance, Hash, Nonce};

/// Protocol asset metadata
#[derive(Debug, Clone, Default)]
pub struct AssetEntry {
    pub symbol: [u8; 12],
    pub decimals: u8,
    pub issuer: Address,
    pub total_supply: Balance,
    pub status: u8, // 0=Active, 1=Frozen, 2=Delisted
}

/// Protocol balances: (asset_id, address) -> balance
#[derive(Debug, Clone, Default)]
pub struct BalanceEntry {
    pub balance: Balance,
    pub frozen: Balance,
}

/// Protocol allowances: (asset_id, owner, spender) -> allowance
#[derive(Debug, Clone, Default)]
pub struct AllowanceEntry {
    pub amount: Balance,
    pub expires_at: u64,
}

/// Shielded viewing keys: address -> encrypted key material
#[derive(Debug, Clone, Default)]
pub struct ViewingKeyEntry {
    pub encrypted_key: Vec<u8>,
    pub nullifier_key_hash: Hash,
}

/// Agent registrations
#[derive(Debug, Clone, Default)]
pub struct AgentEntry {
    pub owner: Address,
    pub name: Vec<u8>,
    pub registered_at: u64,
}

/// Agent nonces: (owner, agent_id) -> nonce
#[derive(Debug, Clone, Default)]
pub struct AgentNonceEntry {
    pub nonce: Nonce,
}

/// EVM accounts
#[derive(Debug, Clone, Default)]
pub struct EvmAccountEntry {
    pub nonce: u64,
    pub balance: u128,
    pub code_hash: Option<Hash>,
}

/// Bridge pending operations
#[derive(Debug, Clone, Default)]
pub struct BridgeOpEntry {
    pub source_chain: u64,
    pub target_address: Address,
    pub amount: Balance,
    pub status: u8, // 0=pending, 1=confirmed, 2=failed
}

/// Fee currency registry: asset_id -> currency_info
#[derive(Debug, Clone, Default)]
pub struct FeeCurrencyEntry {
    pub symbol: [u8; 12],
    pub oracle_price_key: Option<Hash>,
    pub enabled: bool,
}

/// Table descriptor for database registration
#[derive(Debug, Clone)]
pub struct TableDef {
    pub name: &'static str,
    pub description: &'static str,
}

/// All 34 table definitions per spec §10.2
pub fn all_tables() -> &'static [TableDef] {
    &[
        // Protocol layer
        TableDef { name: "protocol_assets", description: "Asset metadata" },
        TableDef { name: "protocol_balances", description: "Protocol balance entries" },
        TableDef { name: "protocol_allowances", description: "Allowance entries" },
        // Shielded pool
        TableDef { name: "shielded_merkle_tree", description: "Merkle tree nodes" },
        TableDef { name: "shielded_nullifiers", description: "Spent nullifiers (never pruned)" },
        TableDef { name: "shielded_commitments", description: "Encrypted note commitments" },
        TableDef { name: "shielded_viewing_keys", description: "User viewing key mappings" },
        // Agent layer
        TableDef { name: "agent_registrations", description: "Agent identity records" },
        TableDef { name: "agent_balances", description: "Agent sub-account balances" },
        TableDef { name: "agent_nonces", description: "Agent sequence numbers" },
        // EVM layer
        TableDef { name: "evm_accounts", description: "EVM account metadata" },
        TableDef { name: "evm_contracts", description: "Contract bytecode" },
        TableDef { name: "evm_storage", description: "Contract storage slots" },
        // Bridge
        TableDef { name: "bridge_pending_ops", description: "Pending bridge operations" },
        // Consensus
        TableDef { name: "consensus_blocks", description: "Block data by height" },
        TableDef { name: "consensus_state", description: "State snapshots by height" },
        // Metadata
        TableDef { name: "metadata_chain_id", description: "Chain identifier" },
        TableDef { name: "metadata_validators", description: "Validator set" },
        TableDef { name: "metadata_compliance", description: "Compliance policy registry" },
        TableDef { name: "metadata_agents", description: "Agent status index" },
        // Receipts and logs
        TableDef { name: "receipts", description: "Transaction receipts" },
        TableDef { name: "logs", description: "Event logs" },
        TableDef { name: "memos", description: "Transaction memos" },
        // Fee and oracle
        TableDef { name: "fee_currency_registry", description: "Fee currency metadata" },
        TableDef { name: "oracle_prices", description: "Oracle price feeds" },
        TableDef { name: "oracle_validator_info", description: "Oracle validator status" },
        // Governance
        TableDef { name: "governance_proposals", description: "Governance proposals" },
        TableDef { name: "vote_delegations", description: "Vote delegation records" },
        // Sponsorship
        TableDef { name: "sponsor_auths", description: "Fee sponsor authorizations" },
        TableDef { name: "sponsor_pools", description: "Fee sponsor pools" },
        TableDef { name: "sponsor_daily_usage", description: "Daily sponsor usage" },
        // Security
        TableDef { name: "session_keys", description: "Session key mappings" },
        TableDef { name: "multi_sig_configs", description: "Multi-sig configurations" },
        TableDef { name: "social_recovery_configs", description: "Social recovery guardians" },
    ]
}

/// Look up a table by name
pub fn find_table(name: &str) -> Option<&'static TableDef> {
    all_tables().iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_count() {
        assert_eq!(all_tables().len(), 34);
    }

    #[test]
    fn test_table_names_unique() {
        for i in 0..all_tables().len() {
            for j in (i + 1)..all_tables().len() {
                assert_ne!(
                    all_tables()[i].name,
                    all_tables()[j].name,
                    "duplicate table: {}",
                    all_tables()[i].name
                );
            }
        }
    }

    #[test]
    fn test_find_table_exists() {
        assert!(find_table("protocol_balances").is_some());
        assert!(find_table("shielded_nullifiers").is_some());
        assert!(find_table("nonexistent").is_none());
    }

    #[test]
    fn test_table_categories_covered() {
        let names: Vec<_> = all_tables().iter().map(|t| t.name).collect();
        // Protocol
        assert!(names.contains(&"protocol_assets"));
        assert!(names.contains(&"protocol_balances"));
        assert!(names.contains(&"protocol_allowances"));
        // Shielded
        assert!(names.contains(&"shielded_merkle_tree"));
        assert!(names.contains(&"shielded_nullifiers"));
        assert!(names.contains(&"shielded_commitments"));
        assert!(names.contains(&"shielded_viewing_keys"));
        // Agent
        assert!(names.contains(&"agent_registrations"));
        assert!(names.contains(&"agent_balances"));
        assert!(names.contains(&"agent_nonces"));
        // EVM
        assert!(names.contains(&"evm_accounts"));
        assert!(names.contains(&"evm_contracts"));
        assert!(names.contains(&"evm_storage"));
        // Bridge
        assert!(names.contains(&"bridge_pending_ops"));
        // Consensus
        assert!(names.contains(&"consensus_blocks"));
        assert!(names.contains(&"consensus_state"));
        // Metadata
        assert!(names.contains(&"metadata_chain_id"));
        assert!(names.contains(&"metadata_validators"));
        assert!(names.contains(&"metadata_compliance"));
        assert!(names.contains(&"metadata_agents"));
        // Receipts
        assert!(names.contains(&"receipts"));
        assert!(names.contains(&"logs"));
        assert!(names.contains(&"memos"));
        // Fee/oracle
        assert!(names.contains(&"fee_currency_registry"));
        assert!(names.contains(&"oracle_prices"));
        assert!(names.contains(&"oracle_validator_info"));
        // Governance
        assert!(names.contains(&"governance_proposals"));
        assert!(names.contains(&"vote_delegations"));
        // Sponsorship
        assert!(names.contains(&"sponsor_auths"));
        assert!(names.contains(&"sponsor_pools"));
        assert!(names.contains(&"sponsor_daily_usage"));
        // Security
        assert!(names.contains(&"session_keys"));
        assert!(names.contains(&"multi_sig_configs"));
        assert!(names.contains(&"social_recovery_configs"));
    }
}

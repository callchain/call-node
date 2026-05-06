use call_primitives::{Address, AssetId};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::logging::audit::AuditLog;
use crate::logging::helpers::format_timestamp_for_compliance;

/// Compliance report entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceReportEntry {
    pub timestamp: String,
    pub block_height: u64,
    pub tx_hash: String,
    pub tx_type: String,
    pub asset_id: AssetId,
    pub asset_symbol: String,
    pub from_address: String,
    pub to_address: String,
    pub amount: String,
    pub fee: String,
    pub agent_id: Option<String>,
    pub compliance_status: String,
}

/// Generate compliance CSV report for an asset and address range.
///
/// `genesis_time` is the chain genesis Unix timestamp; `block_time_secs` is
/// the per-block interval used to derive a block's approximate timestamp.
/// `asset_symbol` is the human-readable symbol for the asset (e.g. "CALL").
pub fn export_compliance_report(
    audit_log: &AuditLog,
    asset_id: AssetId,
    asset_symbol: &str,
    genesis_time: u64,
    block_time_secs: u64,
    address_filter: Option<Address>,
) -> Vec<ComplianceReportEntry> {
    let mut entries = Vec::new();

    for audit in &audit_log.entries {
        // Filter by asset if specified in before/after state
        let before_asset = audit.before_state.get(asset_id.to_string());
        let after_asset = audit.after_state.get(asset_id.to_string());

        if before_asset.is_none() && after_asset.is_none() {
            continue;
        }

        // Filter by address if specified
        if let Some(addr) = address_filter {
            let from_match = audit.fee_payer.map(|a| a == addr).unwrap_or(false);
            if !from_match {
                continue;
            }
        }

        let amount = after_asset
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_sub(before_asset.and_then(|v| v.as_u64()).unwrap_or(0));

        // Derive block timestamp from block height
        let block_timestamp = genesis_time
            .checked_add(audit.block_height.saturating_mul(block_time_secs))
            .unwrap_or(genesis_time);

        let timestamp = SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(block_timestamp))
            .map(|t| format_timestamp_for_compliance(&t))
            .unwrap_or_default();

        entries.push(ComplianceReportEntry {
            timestamp,
            block_height: audit.block_height,
            tx_hash: format!("{:?}", audit.tx_hash),
            tx_type: audit.tx_type.clone(),
            asset_id,
            asset_symbol: asset_symbol.into(),
            from_address: audit
                .fee_payer
                .map(|a| format!("{a:?}"))
                .unwrap_or_default(),
            to_address: String::new(),
            amount: amount.to_string(),
            fee: "0".into(),
            agent_id: audit.agent_id.clone(),
            compliance_status: "verified".into(),
        });
    }

    entries
}

/// Convert report to CSV string
pub fn report_to_csv(entries: &[ComplianceReportEntry]) -> String {
    let mut csv = String::from(
        "timestamp,block_height,tx_hash,tx_type,asset_id,asset_symbol,from_address,to_address,amount,fee,agent_id,compliance_status\n",
    );

    for entry in entries {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            entry.timestamp,
            entry.block_height,
            entry.tx_hash,
            entry.tx_type,
            entry.asset_id,
            entry.asset_symbol,
            entry.from_address,
            entry.to_address,
            entry.amount,
            entry.fee,
            entry.agent_id.as_deref().unwrap_or(""),
            entry.compliance_status,
        ));
    }

    csv
}

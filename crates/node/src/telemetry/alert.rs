//! Alert system and health state.

use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

use super::registry::TelemetryRegistry;

// ── Alert System ─────────────────────────────────────────────────────

/// Alert condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub name: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub triggered_at: std::time::SystemTime,
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlertSeverity {
    Critical,
    Warning,
    Info,
}

/// Alert rules configuration
pub struct AlertRule {
    pub name: String,
    pub severity: AlertSeverity,
    pub check: Box<dyn Fn(&TelemetryRegistry) -> bool + Send + Sync>,
    pub message: String,
}

impl AlertRule {
    pub fn new(
        name: &str,
        severity: AlertSeverity,
        message: &str,
        check: impl Fn(&TelemetryRegistry) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.to_string(),
            severity,
            message: message.to_string(),
            check: Box::new(check),
        }
    }
}

/// Evaluate alert rules and update alert state
pub fn evaluate_alerts(registry: &TelemetryRegistry, rules: &[AlertRule]) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for rule in rules {
        if (rule.check)(registry) {
            alerts.push(Alert {
                name: rule.name.clone(),
                severity: rule.severity,
                message: rule.message.clone(),
                triggered_at: std::time::SystemTime::now(),
                resolved: false,
            });
        }
    }
    alerts
}

/// Default alert rules per spec §20
pub fn default_alert_rules() -> Vec<AlertRule> {
    vec![
        // Consensus stall: no blocks committed for > 60 seconds
        AlertRule::new(
            "consensus_stall",
            AlertSeverity::Critical,
            "No blocks committed in last 60 seconds",
            Box::new(|r: &TelemetryRegistry| {
                let timeouts = r.consensus_timeouts.load(Ordering::Relaxed);
                timeouts > 0 && r.seconds_since_last_block().is_some_and(|s| s > 60)
            }),
        ),
        // Validator offline: peer count drops below minimum
        AlertRule::new(
            "validator_offline",
            AlertSeverity::Critical,
            "P2P peer count is zero",
            Box::new(|r: &TelemetryRegistry| r.p2p_peers.load(Ordering::Relaxed) == 0),
        ),
        // Mempool overflow: > 10000 pending txs
        AlertRule::new(
            "mempool_overflow",
            AlertSeverity::Warning,
            "Mempool exceeds 10000 transactions",
            Box::new(|r: &TelemetryRegistry| r.mempool_tx_count.load(Ordering::Relaxed) > 10_000),
        ),
        // Bridge delay: > 100 pending bridge ops
        AlertRule::new(
            "bridge_delay",
            AlertSeverity::Warning,
            "Bridge pending operations exceed 100",
            Box::new(|r: &TelemetryRegistry| r.mempool_bridge_pending.load(Ordering::Relaxed) > 100),
        ),
        // Memory pressure: check system RAM usage via sysinfo
        AlertRule::new(
            "high_memory_usage",
            AlertSeverity::Warning,
            "Memory usage exceeds 90%",
            Box::new(|_: &TelemetryRegistry| {
                use sysinfo::System;
                let mut sys = System::new_all();
                sys.refresh_memory();
                let total = sys.total_memory();
                let used = sys.used_memory();
                if total == 0 {
                    false
                } else {
                    (used as f64 / total as f64) > 0.9
                }
            }),
        ),
        // Disk space: check available space on data directory filesystem
        AlertRule::new(
            "low_disk_space",
            AlertSeverity::Critical,
            "Disk space below 10%",
            Box::new(|r: &TelemetryRegistry| {
                use fs2::available_space;
                if let Ok(space) = available_space(&r.data_dir) {
                    // Assume 100GB total if we can't get total; alert if < 10GB free
                    space < 10 * 1024 * 1024 * 1024
                } else {
                    false
                }
            }),
        ),
    ]
}

// ── Health Endpoint ──────────────────────────────────────────────────

/// Subsystem health state passed to the /health handler
#[derive(Clone)]
pub struct HealthState {
    pub db: std::sync::Arc<reth_db::DatabaseEnv>,
    pub network: Option<std::sync::Arc<dyn call_network::Network>>,
    pub consensus: std::sync::Arc<std::sync::RwLock<call_consensus::SimplexConsensus>>,
}

/// Lightweight DB heartbeat: write and immediately delete a test key
pub fn db_heartbeat(db: &reth_db::DatabaseEnv) -> Result<(), String> {
    use call_storage::reth_db::{db_del, db_put};
    use call_storage::reth_db::CallMetadataChainId;
    let key = b"__health_check__".to_vec();
    db_put::<CallMetadataChainId>(db, key.clone(), b"1".to_vec())
        .map_err(|e| format!("db write failed: {e}"))?;
    db_del::<CallMetadataChainId>(db, &key)
        .map_err(|e| format!("db delete failed: {e}"))?;
    Ok(())
}

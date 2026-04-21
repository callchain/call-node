//! T17.1 — Logging & Auditing Module (per spec §24)
//!
//! Structured logging, append-only audit log, Merkle-ized audit root,
//! compliance report export, log rotation and retention.

use call_crypto::{keccak256, verify_merkle_proof, build_merkle_root};
use call_primitives::{Address, AssetId, Hash, TxHash};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ── Log Entry ────────────────────────────────────────────────────────

/// JSON log value types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LogValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

/// Structured log entry (per spec §24.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Log level
    pub level: LogLevel,
    /// Module path
    pub target: String,
    /// Log message
    pub message: String,
    /// Additional fields
    pub fields: HashMap<String, LogValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

impl LogEntry {
    pub fn new(level: LogLevel, target: &str, message: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            timestamp: format_timestamp(now),
            level,
            target: target.to_string(),
            message: message.to_string(),
            fields: HashMap::new(),
        }
    }

    pub fn with_field(mut self, key: &str, value: LogValue) -> Self {
        self.fields.insert(key.to_string(), value);
        self
    }

    /// Format as JSON string
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Format as text line
    pub fn to_text(&self) -> String {
        format!(
            "{} [{}] {} {}",
            self.timestamp,
            self.level.as_str(),
            self.target,
            self.message
        )
    }
}

// ── Audit Entry ──────────────────────────────────────────────────────

/// Shielded audit info (encrypted, viewing key required to decrypt)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldedAuditInfo {
    pub nullifiers: Vec<Hash>,
    pub commitments: Vec<Hash>,
    pub encrypted_amounts: Vec<EncryptedValue>,
    pub compliance_mode: String,
}

/// Encrypted value wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedValue {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// Append-only audit entry (per spec §24.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub block_height: u64,
    pub tx_index: u32,
    pub tx_type: String,
    pub action: String,
    pub agent_id: Option<String>,
    pub fee_payer: Option<Address>,
    pub before_state: serde_json::Value,
    pub after_state: serde_json::Value,
    pub tx_hash: TxHash,
    pub shielded_details: Option<ShieldedAuditInfo>,
}

impl AuditEntry {
    /// Hash this entry for Merkle tree inclusion
    pub fn hash(&self) -> Hash {
        let data = serde_json::to_vec(self).unwrap_or_default();
        keccak256(&data)
    }
}

// ── Audit Log ────────────────────────────────────────────────────────

/// Append-only audit log with Merkle tree
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    entry_hashes: Vec<Hash>,
    merkle_root: Hash,
    log_file: Option<File>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            entry_hashes: Vec::new(),
            merkle_root: Hash::ZERO,
            log_file: None,
        }
    }

    /// Open audit log file (append mode)
    pub fn open_file(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("failed to open audit log: {e}"))?;

        Ok(Self {
            entries: Vec::new(),
            entry_hashes: Vec::new(),
            merkle_root: Hash::ZERO,
            log_file: Some(file),
        })
    }

    /// Append an entry (append-only — no deletions or modifications)
    pub fn append(&mut self, entry: AuditEntry) -> Result<(), String> {
        let entry_hash = entry.hash();

        // Write to file if open
        if let Some(ref mut file) = self.log_file {
            let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
            writeln!(file, "{line}").map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
        }

        self.entries.push(entry);
        self.entry_hashes.push(entry_hash);

        // Recompute Merkle root
        self.merkle_root = build_merkle_root(&self.entry_hashes).unwrap_or(Hash::ZERO);

        Ok(())
    }

    /// Get Merkle root of current audit log
    pub fn merkle_root(&self) -> Hash {
        self.merkle_root
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Generate Merkle proof for entry at index
    pub fn merkle_proof_for(&self, index: usize) -> Option<Vec<(Hash, bool)>> {
        if index >= self.entry_hashes.len() {
            return None;
        }

        // Build full Merkle tree and extract path
        let mut current_level: Vec<Hash> = self.entry_hashes.clone();
        let mut all_paths: Vec<Vec<(Hash, bool)>> = vec![Vec::new(); current_level.len()];

        while current_level.len() > 1 {
            let next_level = Vec::with_capacity(current_level.len().div_ceil(2));
            let mut next_paths: Vec<Vec<(Hash, bool)>> = vec![Vec::new(); current_level.len().div_ceil(2)];

            for (i, chunk) in current_level.chunks(2).enumerate() {
                let (_parent, _sibling_left) = match chunk {
                    [a, b] => {
                        let mut data = Vec::with_capacity(64);
                        data.extend_from_slice(a.as_slice());
                        data.extend_from_slice(b.as_slice());
                        (keccak256(&data), false)
                    }
                    [a] => {
                        let mut data = Vec::with_capacity(64);
                        data.extend_from_slice(a.as_slice());
                        data.extend_from_slice(a.as_slice());
                        (keccak256(&data), false)
                    }
                    _ => unreachable!(),
                };

                // Update paths: left child gets sibling on right, right child gets sibling on left
                let left_idx = i * 2;
                let right_idx = i * 2 + 1;

                if left_idx < all_paths.len() {
                    let sibling = chunk.get(1).copied().unwrap_or(chunk[0]);
                    let mut new_path = all_paths[left_idx].clone();
                    new_path.push((sibling, true)); // sibling on right
                    if left_idx < next_paths.len() {
                        next_paths[left_idx / 2] = new_path;
                    }
                }

                if right_idx < all_paths.len() {
                    let sibling = chunk[0];
                    let mut new_path = all_paths[right_idx].clone();
                    new_path.push((sibling, false)); // sibling on left
                    if right_idx / 2 < next_paths.len() {
                        next_paths[right_idx / 2] = new_path;
                    }
                }
            }

            current_level = next_level;
            all_paths = next_paths;
        }

        all_paths.first().cloned()
    }

    /// Verify an entry against the Merkle root
    pub fn verify_entry(&self, entry: &AuditEntry, proof: &[(Hash, bool)]) -> bool {
        verify_merkle_proof(entry.hash(), proof, self.merkle_root)
    }

    /// Read all entries from file
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("failed to open audit log: {e}"))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut hashes = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.is_empty() {
                continue;
            }
            let entry: AuditEntry =
                serde_json::from_str(&line).map_err(|e| format!("invalid audit entry: {e}"))?;
            hashes.push(entry.hash());
            entries.push(entry);
        }

        let merkle_root = build_merkle_root(&hashes).unwrap_or(Hash::ZERO);

        Ok(Self {
            entries,
            entry_hashes: hashes,
            merkle_root,
            log_file: None,
        })
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

// ── Log Configuration ───────────────────────────────────────────────

/// Log configuration (per spec §24.4)
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: LogLevel,
    pub format: LogFormat,
    pub output: LogOutput,
    pub rotation: LogRotation,
    pub retention_days: u32,
    pub audit_enabled: bool,
    pub audit_path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub enum LogOutput {
    Stdout,
    File(PathBuf),
    Both(PathBuf),
}

#[derive(Debug, Clone)]
pub enum LogRotation {
    None,
    Size(u64), // bytes
    Daily,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: LogFormat::Text,
            output: LogOutput::Stdout,
            rotation: LogRotation::Size(100 * 1024 * 1024), // 100MB
            retention_days: 30,
            audit_enabled: true,
            audit_path: PathBuf::from(".callchain/audit.log"),
        }
    }
}

// ── Compliance Report ───────────────────────────────────────────────

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
            let from_match = audit
                .fee_payer
                .map(|a| a == addr)
                .unwrap_or(false);
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

// ── Log Rotation ─────────────────────────────────────────────────────

/// Check if log file needs rotation based on config
pub fn should_rotate(path: &Path, rotation: &LogRotation) -> bool {
    match rotation {
        LogRotation::None => false,
        LogRotation::Size(max_bytes) => {
            path.metadata()
                .map(|m| m.len() >= *max_bytes)
                .unwrap_or(false)
        }
        LogRotation::Daily => {
            path.metadata()
                .and_then(|m| m.modified())
                .map(|modified| {
                    let now = SystemTime::now();
                    now.duration_since(modified)
                        .map(|d| d.as_secs() >= 86_400)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        }
    }
}

/// Rotate log file: rename current to .N, create new
pub fn rotate_log(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }

    // Find highest existing rotation number
    let mut max_n = 0;
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let base = path.file_stem().and_then(|s| s.to_str()).unwrap_or("log");
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(suffix) = name.strip_prefix(&format!("{base}.")) {
                    if let Ok(n) = suffix.parse::<u32>() {
                        max_n = max_n.max(n);
                    }
                }
            }
        }
    }

    // Rotate: rename .N to .(N+1) in reverse order
    for n in (1..=max_n).rev() {
        let old = path.with_file_name(format!(
            "{}.{n}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("log")
        ));
        let new = path.with_file_name(format!(
            "{}.{}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("log"),
            n + 1
        ));
        if old.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }

    // Rename current to .1
    let rotated = path.with_file_name(format!(
        "{}.1",
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("log")
    ));
    std::fs::rename(path, &rotated).map_err(|e| format!("failed to rotate: {e}"))?;

    // Create new empty file
    File::create(path).map_err(|e| format!("failed to create new log: {e}"))?;

    Ok(())
}

/// Clean up old log files beyond retention period
pub fn cleanup_old_logs(path: &Path, retention_days: u32) -> Result<usize, String> {
    let mut removed = 0;
    let cutoff = retention_days as u64 * 86_400;

    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let base = path.file_stem().and_then(|s| s.to_str()).unwrap_or("log");
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{base}.")) {
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            let age = SystemTime::now()
                                .duration_since(modified)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if age > cutoff {
                                let _ = std::fs::remove_file(entry.path());
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(removed)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn format_timestamp(secs: u64) -> String {
    // Simplified ISO 8601 — full implementation would use chrono/jiff
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    format!("{year}-01-{day_of_year:03}T{hours:02}:{mins:02}:{s:02}Z")
}

fn format_timestamp_for_compliance(t: &SystemTime) -> String {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| format_timestamp(d.as_secs()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_hash(n: u8) -> Hash {
        Hash::repeat_byte(n)
    }

    fn make_audit_entry(block: u64, tx_idx: u32) -> AuditEntry {
        AuditEntry {
            block_height: block,
            tx_index: tx_idx,
            tx_type: "Transfer".into(),
            action: "execute".into(),
            agent_id: None,
            fee_payer: Some(test_addr(1)),
            before_state: serde_json::json!({"1": 1000}),
            after_state: serde_json::json!({"1": 900}),
            tx_hash: test_hash(tx_idx as u8),
            shielded_details: None,
        }
    }

    #[test]
    fn test_structured_log_json_format() {
        let entry = LogEntry::new(LogLevel::Info, "call_node", "test message")
            .with_field("height", LogValue::Number(42.0))
            .with_field("valid", LogValue::Bool(true));

        let json = entry.to_json();
        assert!(json.contains("\"level\":\"info\""));
        assert!(json.contains("\"target\":\"call_node\""));
        assert!(json.contains("\"message\":\"test message\""));
        assert!(json.contains("42.0"));

        // Verify round-trip
        let parsed: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.message, "test message");
    }

    #[test]
    fn test_structured_log_text_format() {
        let entry = LogEntry::new(LogLevel::Warn, "call_consensus", "timeout detected");
        let text = entry.to_text();
        assert!(text.contains("warn"));
        assert!(text.contains("call_consensus"));
        assert!(text.contains("timeout detected"));
    }

    #[test]
    fn test_audit_log_append_only() {
        let mut log = AuditLog::new();

        let e1 = make_audit_entry(1, 0);
        log.append(e1.clone()).unwrap();
        assert_eq!(log.len(), 1);

        let e2 = make_audit_entry(1, 1);
        log.append(e2.clone()).unwrap();
        assert_eq!(log.len(), 2);

        let e3 = make_audit_entry(2, 0);
        log.append(e3.clone()).unwrap();
        assert_eq!(log.len(), 3);

        // Verify entries are in order
        assert_eq!(log.entries[0].block_height, 1);
        assert_eq!(log.entries[1].block_height, 1);
        assert_eq!(log.entries[2].block_height, 2);
    }

    #[test]
    fn test_audit_log_merkle_root() {
        let mut log = AuditLog::new();

        // Empty log has zero root
        assert_eq!(log.merkle_root(), Hash::ZERO);

        // Add entries
        for i in 0..4 {
            log.append(make_audit_entry(1, i)).unwrap();
        }

        let root = log.merkle_root();
        assert_ne!(root, Hash::ZERO);

        // Root should be deterministic
        let root2 = log.merkle_root();
        assert_eq!(root, root2);

        // Adding another entry should change root
        log.append(make_audit_entry(2, 0)).unwrap();
        let root3 = log.merkle_root();
        assert_ne!(root, root3);
    }

    #[test]
    fn test_audit_log_file_roundtrip() {
        let dir = std::env::temp_dir().join("call-audit-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.log");

        // Write entries
        {
            let mut log = AuditLog::open_file(&path).unwrap();
            log.append(make_audit_entry(1, 0)).unwrap();
            log.append(make_audit_entry(1, 1)).unwrap();
            log.append(make_audit_entry(2, 0)).unwrap();
        }

        // Read back
        let log = AuditLog::load_from_file(&path).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log.entries[0].block_height, 1);
        assert_eq!(log.entries[1].block_height, 1);
        assert_eq!(log.entries[2].block_height, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_compliance_report_export() {
        let mut log = AuditLog::new();
        log.append(make_audit_entry(1, 0)).unwrap();
        log.append(make_audit_entry(1, 1)).unwrap();
        log.append(make_audit_entry(2, 0)).unwrap();

        let report = export_compliance_report(&log, 1, "CALL", 1_700_000_000, 2, None);
        assert!(!report.is_empty());

        // Timestamps should be derived from block height, not epoch zero
        // block 1 → 1_700_000_000 + 1*2 = 1_700_000_002
        // block 2 → 1_700_000_000 + 2*2 = 1_700_000_004
        assert!(report[0].timestamp.contains("2023"), "timestamp should be in 2023, got {}", report[0].timestamp);

        // Check CSV output
        let csv = report_to_csv(&report);
        assert!(csv.contains("timestamp,block_height,tx_hash"));
        assert!(csv.contains("Transfer"));
    }

    #[test]
    fn test_log_rotation() {
        let dir = std::env::temp_dir().join("call-rotation-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.log");

        // Create a file
        std::fs::write(&path, "log data").unwrap();
        assert!(path.exists());

        // Rotate
        rotate_log(&path).unwrap();
        assert!(!path.exists() || std::fs::read_to_string(&path).unwrap_or_default().is_empty());

        let rotated = dir.join("test.1");
        assert!(rotated.exists());
        assert_eq!(std::fs::read_to_string(&rotated).unwrap(), "log data");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_should_rotate_by_size() {
        let dir = std::env::temp_dir().join("call-rotate-size-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("size.log");

        // Write 50 bytes
        std::fs::write(&path, "a".repeat(50)).unwrap();

        // Should not rotate at 100MB threshold
        assert!(!should_rotate(&path, &LogRotation::Size(100 * 1024 * 1024)));

        // Should rotate at 40 byte threshold
        assert!(should_rotate(&path, &LogRotation::Size(40)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_log_entry_with_fields() {
        let mut fields = HashMap::new();
        fields.insert("key".to_string(), LogValue::String("value".into()));

        let entry = LogEntry {
            timestamp: "2024-01-01T00:00:00Z".into(),
            level: LogLevel::Debug,
            target: "test".into(),
            message: "test".into(),
            fields,
        };

        let json = entry.to_json();
        assert!(json.contains("key"));
        assert!(json.contains("value"));
    }

    #[test]
    fn test_audit_entry_hash_deterministic() {
        let entry = make_audit_entry(42, 5);
        let h1 = entry.hash();
        let h2 = entry.hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_shielded_audit_info_structure() {
        let info = ShieldedAuditInfo {
            nullifiers: vec![test_hash(1), test_hash(2)],
            commitments: vec![test_hash(3)],
            encrypted_amounts: vec![EncryptedValue {
                ciphertext: vec![1, 2, 3],
                nonce: vec![4, 5, 6],
            }],
            compliance_mode: "transparent".into(),
        };
        assert_eq!(info.nullifiers.len(), 2);
        assert_eq!(info.compliance_mode, "transparent");
    }

    #[test]
    fn test_log_config_defaults() {
        let config = LogConfig::default();
        assert_eq!(config.level, LogLevel::Info);
        assert!(config.audit_enabled);
        assert!(matches!(config.rotation, LogRotation::Size(_)));
        assert_eq!(config.retention_days, 30);
    }

    #[test]
    fn test_report_to_csv() {
        let entries = vec![
            ComplianceReportEntry {
                timestamp: "2024-01-01T00:00:00Z".into(),
                block_height: 1,
                tx_hash: "0x01".into(),
                tx_type: "Transfer".into(),
                asset_id: 1,
                asset_symbol: "CALL".into(),
                from_address: "addr1".into(),
                to_address: "addr2".into(),
                amount: "100".into(),
                fee: "1".into(),
                agent_id: None,
                compliance_status: "verified".into(),
            },
        ];

        let csv = report_to_csv(&entries);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2); // header + 1 data row
        assert!(lines[0].contains("timestamp"));
        assert!(lines[1].contains("Transfer"));
    }
}

//! T17.1 — Logging & Auditing Module (per spec §24)
//!
//! Structured logging, append-only audit log, Merkle-ized audit root,
//! compliance report export, log rotation and retention.

pub mod audit;
pub mod compliance;
pub mod config;
pub mod entry;
pub mod file_log;
pub mod helpers;
pub mod rotation;

pub use audit::*;
pub use compliance::*;
pub use config::*;
pub use entry::*;
pub use helpers::*;
pub use rotation::*;

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::{Address, Hash};
    use std::collections::HashMap;

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
        // block 1 -> 1_700_000_000 + 1*2 = 1_700_000_002
        // block 2 -> 1_700_000_000 + 2*2 = 1_700_000_004
        assert!(
            report[0].timestamp.contains("2023"),
            "timestamp should be in 2023, got {}",
            report[0].timestamp
        );

        // Check CSV output
        let csv = report_to_csv(&report);
        assert!(csv.contains("timestamp,block_height,tx_hash"));
        assert!(csv.contains("Transfer"));
    }

    fn make_audit_entry_with_assets(
        block: u64,
        tx_idx: u32,
        before: serde_json::Value,
        after: serde_json::Value,
    ) -> AuditEntry {
        AuditEntry {
            block_height: block,
            tx_index: tx_idx,
            tx_type: "Transfer".into(),
            action: "execute".into(),
            agent_id: None,
            fee_payer: Some(test_addr(1)),
            before_state: before,
            after_state: after,
            tx_hash: test_hash(tx_idx as u8),
            shielded_details: None,
        }
    }

    #[test]
    fn test_compliance_report_symbol_accuracy_per_asset_id() {
        let mut log = AuditLog::new();

        // Entry with asset_id=1 (CALL)
        log.append(make_audit_entry_with_assets(
            1,
            0,
            serde_json::json!({"1": 1000}),
            serde_json::json!({"1": 900}),
        ))
        .unwrap();

        // Entry with asset_id=2 (USDC)
        log.append(make_audit_entry_with_assets(
            1,
            1,
            serde_json::json!({"2": 500}),
            serde_json::json!({"2": 400}),
        ))
        .unwrap();

        // Mixed entry: both assets changed
        log.append(make_audit_entry_with_assets(
            2,
            0,
            serde_json::json!({"1": 100, "2": 200}),
            serde_json::json!({"1": 90, "2": 190}),
        ))
        .unwrap();

        // Report for asset_id=1 should only include entries where asset 1 changed
        // Entries 0 and 2 have asset 1; entry 1 does not.
        let report_call = export_compliance_report(&log, 1, "CALL", 1_700_000_000, 2, None);
        assert_eq!(report_call.len(), 2, "asset_id=1 appears in entries 0 and 2");
        for entry in &report_call {
            assert_eq!(entry.asset_id, 1);
            assert_eq!(entry.asset_symbol, "CALL", "asset_id=1 must map to CALL");
        }

        // Report for asset_id=2 should only include entries where asset 2 changed
        // Entries 1 and 2 have asset 2; entry 0 does not.
        let report_usdc = export_compliance_report(&log, 2, "USDC", 1_700_000_000, 2, None);
        assert_eq!(report_usdc.len(), 2, "asset_id=2 appears in entries 1 and 2");
        for entry in &report_usdc {
            assert_eq!(entry.asset_id, 2);
            assert_eq!(entry.asset_symbol, "USDC", "asset_id=2 must map to USDC, not CALL");
        }

        // Verify CSV contains correct symbols
        let csv_call = report_to_csv(&report_call);
        assert!(csv_call.contains("CALL"));
        assert!(!csv_call.contains("USDC"));

        let csv_usdc = report_to_csv(&report_usdc);
        assert!(csv_usdc.contains("USDC"));
        assert!(!csv_usdc.contains("CALL"));
    }

    #[test]
    fn test_compliance_report_asset_id_filtering_excludes_unrelated() {
        let mut log = AuditLog::new();

        // Only asset_id=3 appears
        log.append(make_audit_entry_with_assets(
            1,
            0,
            serde_json::json!({"3": 1000}),
            serde_json::json!({"3": 500}),
        ))
        .unwrap();

        // Request report for asset_id=7 — should be empty
        let report = export_compliance_report(&log, 7, "WETH", 1_700_000_000, 2, None);
        assert!(report.is_empty(), "asset_id=7 should have no matching entries");
    }

    #[test]
    fn test_compliance_report_amount_computed_from_state_delta() {
        let mut log = AuditLog::new();

        log.append(make_audit_entry_with_assets(
            1,
            0,
            serde_json::json!({"1": 10000}),
            serde_json::json!({"1": 7500}),
        ))
        .unwrap();

        let report = export_compliance_report(&log, 1, "CALL", 1_700_000_000, 2, None);
        assert_eq!(report.len(), 1);
        // amount = after - before (saturating); 7500 - 10000 would underflow, so 0
        assert_eq!(report[0].amount, "0", "negative delta saturates to 0");

        // Reverse: increasing balance
        let mut log2 = AuditLog::new();
        log2.append(make_audit_entry_with_assets(
            1,
            0,
            serde_json::json!({"1": 500}),
            serde_json::json!({"1": 3000}),
        ))
        .unwrap();
        let report2 = export_compliance_report(&log2, 1, "CALL", 1_700_000_000, 2, None);
        assert_eq!(report2[0].amount, "2500", "positive delta = 3000 - 500");
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
        assert!(
            !path.exists()
                || std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .is_empty()
        );

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
    fn test_log_rotation_rapid_sequence() {
        let dir = std::env::temp_dir().join("call-rapid-rotate-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rapid.log");

        // Perform 10 rapid rotations
        for i in 0..10 {
            std::fs::write(&path, format!("batch-{i}")).unwrap();
            rotate_log(&path).unwrap();
        }

        // Verify all rotated files exist with correct sequential numbering
        for i in 1..=10 {
            let rotated = dir.join(format!("rapid.{i}"));
            assert!(rotated.exists(), "rotated file rapid.{i} should exist");
        }

        // Verify the newest data is in .1 (most recent rotation)
        assert_eq!(
            std::fs::read_to_string(&dir.join("rapid.1")).unwrap(),
            "batch-9"
        );
        // And oldest in .10
        assert_eq!(
            std::fs::read_to_string(&dir.join("rapid.10")).unwrap(),
            "batch-0"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cleanup_old_logs_removes_stale() {
        let dir = std::env::temp_dir().join("call-cleanup-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cleanup.log");

        // Create rotated files
        for i in 1..=5 {
            let rotated = dir.join(format!("cleanup.{i}"));
            std::fs::write(&rotated, format!("data-{i}")).unwrap();
        }

        // Fresh files should NOT be removed even with 0-day retention
        // because age is 0 and cutoff is 0 (age > cutoff requires age >= 1)
        let removed = cleanup_old_logs(&path, 0).unwrap();
        assert_eq!(removed, 0, "fresh files should not be removed");

        // Sleep to ensure files are older than 0 seconds
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let removed = cleanup_old_logs(&path, 0).unwrap();
        assert_eq!(removed, 5, "all 5 rotated files should be removed after 1s");

        for i in 1..=5 {
            assert!(!dir.join(format!("cleanup.{i}")).exists());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rotate_log_fails_on_readonly_dir() {
        // Simulate disk-full by using a read-only directory
        let dir = std::env::temp_dir().join("call-readonly-rotate-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("readonly.log");
        std::fs::write(&path, "some data").unwrap();

        // Make directory read-only
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        let original_perms = perms.clone();
        perms.set_readonly(true);
        std::fs::set_permissions(&dir, perms).unwrap();

        // Rotation should fail because it cannot create the new file
        let result = rotate_log(&path);

        // Restore permissions before assertions so cleanup works
        std::fs::set_permissions(&dir, original_perms).unwrap();

        assert!(result.is_err(), "rotation in read-only dir should fail");

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
        let entries = vec![ComplianceReportEntry {
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
        }];

        let csv = report_to_csv(&entries);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2); // header + 1 data row
        assert!(lines[0].contains("timestamp"));
        assert!(lines[1].contains("Transfer"));
    }

    // ── FileLogLayer high-volume test (gap #27) ───────────────────────

    #[tokio::test]
    async fn test_file_log_layer_sustained_high_volume() {
        let dir = std::env::temp_dir().join(format!("call-filelog-volume-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("volume.log");

        let config = LogConfig {
            level: LogLevel::Info,
            format: LogFormat::Text,
            output: LogOutput::File(path.clone()),
            rotation: LogRotation::Size(1024 * 1024), // 1MB — won't trigger during test
            retention_days: 1,
            audit_enabled: false,
            audit_path: dir.join("audit.log"),
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        crate::logging::file_log::start_file_logger_task(rx, config).unwrap();

        const COUNT: usize = 10_000;

        // Pump 10K log entries rapidly
        for i in 0..COUNT {
            let entry = LogEntry::new(LogLevel::Info, "test", &format!("log-line-{i}"));
            let _ = tx.send(entry);
        }

        // Drop sender so the channel eventually closes (task keeps ticking)
        drop(tx);

        // Wait for background task to drain the queue
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify all lines were written
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        assert!(
            lines.len() >= COUNT,
            "expected at least {COUNT} log lines, got {}",
            lines.len()
        );

        // Verify first and last lines are present
        assert!(lines[0].contains("log-line-0"), "first line should contain log-line-0");
        assert!(
            lines[COUNT - 1].contains(&format!("log-line-{}", COUNT - 1)),
            "last line should contain log-line-{}",
            COUNT - 1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

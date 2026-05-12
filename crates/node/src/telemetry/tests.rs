//! Tests for the telemetry system.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::alert::{default_alert_rules, evaluate_alerts, AlertSeverity, HealthState};
use super::registry::TelemetryRegistry;
use super::server::start_metrics_server;

#[test]
fn test_prometheus_metrics_endpoint() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());
    reg.record_block_produced();
    reg.record_block_committed();
    reg.set_mempool_size(50);
    reg.set_p2p_peers(10);
    reg.record_p2p_bytes_sent(1024);
    reg.record_p2p_bytes_received(2048);

    let output = reg.prometheus_output();

    assert!(output.contains("consensus_blocks_produced 1"));
    assert!(output.contains("consensus_blocks_committed 1"));
    assert!(output.contains("mempool_tx_count 50"));
    assert!(output.contains("p2p_peers 10"));
    assert!(output.contains("p2p_bytes_sent 1024"));
    assert!(output.contains("p2p_bytes_received 2048"));
    assert!(output.contains("node_uptime_seconds"));
    assert!(output.contains("# HELP"));
    assert!(output.contains("# TYPE"));
}

#[test]
fn test_consensus_metrics_recorded() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());

    // Simulate consensus activity
    reg.record_block_produced();
    reg.record_block_produced();
    reg.record_block_committed();
    reg.record_consensus_timeout();

    assert_eq!(reg.consensus_blocks_produced.load(Ordering::Relaxed), 2);
    assert_eq!(reg.consensus_blocks_committed.load(Ordering::Relaxed), 1);
    assert_eq!(reg.consensus_rounds.load(Ordering::Relaxed), 2);
    assert_eq!(reg.consensus_timeouts.load(Ordering::Relaxed), 1);

    let output = reg.prometheus_output();
    assert!(output.contains("consensus_blocks_produced 2"));
    assert!(output.contains("consensus_timeouts 1"));
}

#[test]
fn test_mempool_size_metric() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());

    reg.set_mempool_size(100);
    assert_eq!(reg.mempool_tx_count.load(Ordering::Relaxed), 100);

    reg.set_mempool_size(50);
    assert_eq!(reg.mempool_tx_count.load(Ordering::Relaxed), 50);

    reg.record_tx_rejected();
    reg.record_tx_rejected();
    assert_eq!(reg.mempool_tx_rejected.load(Ordering::Relaxed), 2);

    reg.set_bridge_pending(5);
    assert_eq!(reg.mempool_bridge_pending.load(Ordering::Relaxed), 5);

    let output = reg.prometheus_output();
    assert!(output.contains("mempool_tx_count 50"));
    assert!(output.contains("mempool_tx_rejected 2"));
    assert!(output.contains("mempool_bridge_pending 5"));
}

#[test]
fn test_alert_consensus_stall() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());
    let rules = default_alert_rules();

    // Initially no alerts (no timeouts, no blocks — normal startup)
    let alerts = evaluate_alerts(&reg, &rules);
    let stall = alerts.iter().find(|a| a.name == "consensus_stall");
    assert!(stall.is_none(), "should not alert on fresh registry");

    // Record a timeout without any committed blocks → still no alert (never committed)
    reg.record_consensus_timeout();
    let alerts = evaluate_alerts(&reg, &rules);
    let stall = alerts.iter().find(|a| a.name == "consensus_stall");
    assert!(
        stall.is_none(),
        "should not alert when no block ever committed"
    );

    // Commit a block → stall clears
    reg.record_block_committed();
    let alerts = evaluate_alerts(&reg, &rules);
    let stall = alerts.iter().find(|a| a.name == "consensus_stall");
    assert!(
        stall.is_none(),
        "should not alert right after block committed"
    );

    // Simulate stall: pretend the last committed block was 61 seconds ago
    reg.set_seconds_since_last_block_for_test(61);
    let alerts = evaluate_alerts(&reg, &rules);
    let stall = alerts.iter().find(|a| a.name == "consensus_stall");
    assert!(stall.is_some(), "should alert when > 60s since last block");
    let stall = stall.unwrap();
    assert_eq!(stall.severity, AlertSeverity::Critical);
}

#[test]
fn test_alert_mempool_overflow() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());
    let rules = default_alert_rules();

    reg.set_mempool_size(10_001);
    let alerts = evaluate_alerts(&reg, &rules);
    let overflow = alerts.iter().find(|a| a.name == "mempool_overflow");
    assert!(overflow.is_some());
    assert_eq!(overflow.unwrap().severity, AlertSeverity::Warning);

    reg.set_mempool_size(5_000);
    let alerts = evaluate_alerts(&reg, &rules);
    let overflow = alerts.iter().find(|a| a.name == "mempool_overflow");
    assert!(overflow.is_none());
}

#[test]
fn test_telemetry_registry_default() {
    let reg = TelemetryRegistry::default();
    assert_eq!(reg.consensus_blocks_produced.load(Ordering::Relaxed), 0);
    assert_eq!(reg.p2p_peers.load(Ordering::Relaxed), 0);
    assert!(reg.uptime() < Duration::from_secs(1));
}

#[test]
fn test_storage_prune_metrics() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());

    // Initially zero
    assert_eq!(reg.storage_traces_pruned.load(Ordering::Relaxed), 0);
    assert_eq!(reg.storage_receipts_pruned.load(Ordering::Relaxed), 0);
    assert_eq!(reg.storage_bodies_pruned.load(Ordering::Relaxed), 0);
    assert_eq!(reg.storage_snapshots_pruned.load(Ordering::Relaxed), 0);

    // Record prune counters (cumulative from PruneState)
    reg.record_storage_prune(1_500, 3_000, 800, 2);
    assert_eq!(reg.storage_traces_pruned.load(Ordering::Relaxed), 1_500);
    assert_eq!(reg.storage_receipts_pruned.load(Ordering::Relaxed), 3_000);
    assert_eq!(reg.storage_bodies_pruned.load(Ordering::Relaxed), 800);
    assert_eq!(reg.storage_snapshots_pruned.load(Ordering::Relaxed), 2);

    // Update to new cumulative values
    reg.record_storage_prune(3_000, 6_000, 1_600, 5);
    assert_eq!(reg.storage_traces_pruned.load(Ordering::Relaxed), 3_000);
    assert_eq!(reg.storage_receipts_pruned.load(Ordering::Relaxed), 6_000);
    assert_eq!(reg.storage_bodies_pruned.load(Ordering::Relaxed), 1_600);
    assert_eq!(reg.storage_snapshots_pruned.load(Ordering::Relaxed), 5);

    let output = reg.prometheus_output();
    assert!(output.contains("storage_traces_pruned 3000"));
    assert!(output.contains("storage_receipts_pruned 6000"));
    assert!(output.contains("storage_bodies_pruned 1600"));
    assert!(output.contains("storage_snapshots_pruned 5"));
}

#[test]
fn test_telemetry_uptime_increases() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());
    let t1 = reg.uptime();
    thread::sleep(Duration::from_millis(50));
    let t2 = reg.uptime();
    assert!(t2 > t1);
}

#[test]
fn test_register_custom_metric() {
    let reg = TelemetryRegistry::new(std::env::temp_dir());
    let mut labels = HashMap::new();
    labels.insert("asset".to_string(), "CALL".to_string());
    reg.set_gauge("custom_balance", "Custom balance metric", 999.0, labels);

    let output = reg.prometheus_output();
    assert!(output.contains("custom_balance"));
    assert!(output.contains("asset=\"CALL\""));
    assert!(output.contains("999"));
}

// ── Integration tests ────────────────────────────────────────────────

fn dummy_health() -> HealthState {
    let tmp = std::env::temp_dir().join(format!(
        "call-health-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = call_storage::open_db(tmp).unwrap();
    let evm = call_evm::provider::InMemoryStateProvider::new();
    HealthState {
        db: db.db,
        network: None,
        consensus: std::sync::Arc::new(std::sync::RwLock::new(
            call_consensus::SimplexConsensus::new(call_consensus::ConsensusParams::default(), &evm),
        )),
    }
}

#[tokio::test]
async fn test_metrics_http_server() {
    let registry = std::sync::Arc::new(TelemetryRegistry::new(std::env::temp_dir()));
    registry.record_block_produced();
    registry.set_p2p_peers(5);

    let addr = start_metrics_server(registry, dummy_health(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Give server a moment to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .get(format!("http://{addr}/metrics"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(body.contains("consensus_blocks_produced"));
    assert!(body.contains("p2p_peers 5"));
}

#[tokio::test]
async fn test_health_endpoint() {
    let registry = std::sync::Arc::new(TelemetryRegistry::new(std::env::temp_dir()));
    let addr = start_metrics_server(registry, dummy_health(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();

    // With no network and height=0, should be degraded (503)
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body_text = resp.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["status"], "degraded");
    assert!(body["checks"]["db"].as_str().unwrap().contains("ok"));
    assert_eq!(body["checks"]["p2p"], "disabled");
    assert_eq!(body["checks"]["sync"], "not_started");
}

#[tokio::test]
async fn test_metrics_content_type() {
    let registry = std::sync::Arc::new(TelemetryRegistry::new(std::env::temp_dir()));
    let addr = start_metrics_server(registry, dummy_health(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .get(format!("http://{addr}/metrics"))
        .send()
        .await
        .unwrap();

    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/plain"));
    assert!(content_type.contains("version=0.0.4"));
}

#[test]
fn test_opentelemetry_span_recording() {
    let registry = TelemetryRegistry::new(std::env::temp_dir());

    // Verify the registry side-effects work (OTel spans require global init)
    registry.record_block_produced();
    assert_eq!(
        registry.consensus_blocks_produced.load(Ordering::Relaxed),
        1
    );

    registry.set_p2p_peers(10);
    assert_eq!(registry.p2p_peers.load(Ordering::Relaxed), 10);
}

#[test]
fn test_otel_spans_safe_without_global_init() {
    // Ensure OTel span functions are no-ops (not panics) when the global
    // tracer provider has not been initialized (unit-test context).
    let registry = TelemetryRegistry::new(std::env::temp_dir());

    super::record_block_span(&registry, 42, 150);
    super::record_tx_span(&registry, "evm", 25, true);
    super::record_tx_span(&registry, "evm", 30, false);
    super::record_p2p_span(&registry, "sent", "block_announcement", 1024);
    super::record_p2p_span(&registry, "received", "transaction", 512);

    // Rejected tx should still increment the registry counter
    assert_eq!(registry.mempool_tx_rejected.load(Ordering::Relaxed), 1);
}

// ── OpenTelemetry span emission tests (gap #26) ─────────────────────

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use opentelemetry_sdk::export::trace::{ExportResult, SpanData, SpanExporter};
use opentelemetry_sdk::trace::TracerProvider;

#[derive(Debug, Clone)]
struct CaptureExporter {
    spans: Arc<Mutex<Vec<SpanData>>>,
}

impl CaptureExporter {
    fn new() -> Self {
        Self {
            spans: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn get_spans(&self) -> Vec<SpanData> {
        self.spans.lock().unwrap().clone()
    }
}

impl SpanExporter for CaptureExporter {
    fn export(
        &mut self,
        batch: Vec<SpanData>,
    ) -> Pin<Box<dyn std::future::Future<Output = ExportResult> + Send + 'static>> {
        self.spans.lock().unwrap().extend(batch);
        Box::pin(std::future::ready(Ok(())))
    }
}

fn setup_test_tracer() -> (TracerProvider, CaptureExporter) {
    let exporter = CaptureExporter::new();
    let provider = TracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    (provider, exporter)
}

/// Single combined test to avoid OnceLock isolation issues across tests.
/// Verifies block, tx, and p2p spans are all emitted to the collector.
#[test]
fn test_otel_spans_emitted_to_collector() {
    let (provider, exporter) = setup_test_tracer();

    // Set global provider once. If another test already set it, this fails —
    // but we run all assertions in one test so isolation is guaranteed.
    let _ = crate::telemetry::otel::GLOBAL_PROVIDER.set(provider);

    let registry = TelemetryRegistry::new(std::env::temp_dir());
    registry.record_block_produced();
    registry.record_block_committed();

    super::record_block_span(&registry, 42, 150);
    super::record_tx_span(&registry, "evm", 25, true);
    super::record_p2p_span(&registry, "sent", "block_announcement", 1024);

    // Force flush spans
    if let Some(ref provider) = crate::telemetry::otel::GLOBAL_PROVIDER.get() {
        let _ = provider.force_flush();
    }

    let spans = exporter.get_spans();
    assert!(!spans.is_empty(), "spans should be emitted to collector");

    // Block span
    let block_span = spans.iter().find(|s| s.name.as_ref() == "block_produced");
    assert!(block_span.is_some(), "should find a 'block_produced' span");
    let span = block_span.unwrap();
    let height_attr = span.attributes.iter().find(|a| a.key.as_ref() == "block.height");
    assert!(height_attr.is_some());
    assert_eq!(height_attr.unwrap().value, opentelemetry::Value::I64(42));
    let duration_attr = span.attributes.iter().find(|a| a.key.as_ref() == "block.duration_ms");
    assert!(duration_attr.is_some());
    assert_eq!(duration_attr.unwrap().value, opentelemetry::Value::I64(150));

    // Tx span
    let tx_span = spans.iter().find(|s| s.name.as_ref() == "tx_processed");
    assert!(tx_span.is_some(), "should find a 'tx_processed' span");

    // P2P span
    let p2p_span = spans.iter().find(|s| s.name.as_ref() == "p2p_message");
    assert!(p2p_span.is_some(), "should find a 'p2p_message' span");
    let span = p2p_span.unwrap();
    let bytes_attr = span.attributes.iter().find(|a| a.key.as_ref() == "p2p.bytes");
    assert!(bytes_attr.is_some());
    assert_eq!(bytes_attr.unwrap().value, opentelemetry::Value::I64(1024));
}

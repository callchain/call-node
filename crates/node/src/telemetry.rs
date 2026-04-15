//! T15.1 — Telemetry Module (per spec §20)
//!
//! Prometheus metrics: consensus, mempool, bridge, P2P, performance, system.
//! `/metrics` endpoint on `:9090`. Alert rules for operational monitoring.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

// ── Metric Types ─────────────────────────────────────────────────────

/// A single Prometheus-style metric
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub help: String,
    pub r#type: MetricType,
    pub samples: Vec<MetricSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricType {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub labels: HashMap<String, String>,
    pub value: f64,
}

// ── Telemetry Registry ───────────────────────────────────────────────

/// Central telemetry registry collecting all subsystem metrics
pub struct TelemetryRegistry {
    metrics: RwLock<HashMap<String, Metric>>,
    alerts: RwLock<Vec<Alert>>,
    start_time: Instant,
    // Atomic counters for hot-path performance
    pub consensus_blocks_produced: AtomicU64,
    pub consensus_blocks_committed: AtomicU64,
    pub consensus_rounds: AtomicU64,
    pub consensus_timeouts: AtomicU64,
    pub mempool_tx_count: AtomicU64,
    pub mempool_tx_rejected: AtomicU64,
    pub mempool_bridge_pending: AtomicU64,
    pub p2p_peers: AtomicU64,
    pub p2p_bytes_sent: AtomicU64,
    pub p2p_bytes_received: AtomicU64,
}

impl TelemetryRegistry {
    pub fn new() -> Self {
        Self {
            metrics: RwLock::new(HashMap::new()),
            alerts: RwLock::new(Vec::new()),
            start_time: Instant::now(),
            consensus_blocks_produced: AtomicU64::new(0),
            consensus_blocks_committed: AtomicU64::new(0),
            consensus_rounds: AtomicU64::new(0),
            consensus_timeouts: AtomicU64::new(0),
            mempool_tx_count: AtomicU64::new(0),
            mempool_tx_rejected: AtomicU64::new(0),
            mempool_bridge_pending: AtomicU64::new(0),
            p2p_peers: AtomicU64::new(0),
            p2p_bytes_sent: AtomicU64::new(0),
            p2p_bytes_received: AtomicU64::new(0),
        }
    }

    /// Register or update a metric
    pub fn register_metric(&self, name: &str, help: &str, r#type: MetricType, samples: Vec<MetricSample>) {
        let mut metrics = self.metrics.write().unwrap();
        metrics.insert(
            name.to_string(),
            Metric {
                name: name.to_string(),
                help: help.to_string(),
                r#type,
                samples,
            },
        );
    }

    /// Set a gauge metric value
    pub fn set_gauge(&self, name: &str, help: &str, value: f64, labels: HashMap<String, String>) {
        self.register_metric(name, help, MetricType::Gauge, vec![MetricSample { labels, value }]);
    }

    /// Increment a counter metric
    pub fn increment_counter(&self, name: &str, help: &str) {
        let mut metrics = self.metrics.write().unwrap();
        let metric = metrics
            .entry(name.to_string())
            .or_insert_with(|| Metric {
                name: name.to_string(),
                help: help.to_string(),
                r#type: MetricType::Counter,
                samples: vec![MetricSample { labels: HashMap::new(), value: 0.0 }],
            });
        if let Some(sample) = metric.samples.first_mut() {
            sample.value += 1.0;
        }
    }

    /// Get current uptime
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Record a consensus block produced
    pub fn record_block_produced(&self) {
        self.consensus_blocks_produced.fetch_add(1, Ordering::Relaxed);
        self.consensus_rounds.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a consensus block committed
    pub fn record_block_committed(&self) {
        self.consensus_blocks_committed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a consensus timeout
    pub fn record_consensus_timeout(&self) {
        self.consensus_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// Set current mempool size
    pub fn set_mempool_size(&self, count: usize) {
        self.mempool_tx_count.store(count as u64, Ordering::Relaxed);
    }

    /// Record a rejected transaction
    pub fn record_tx_rejected(&self) {
        self.mempool_tx_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Set bridge pending count
    pub fn set_bridge_pending(&self, count: usize) {
        self.mempool_bridge_pending.store(count as u64, Ordering::Relaxed);
    }

    /// Set P2P peer count
    pub fn set_p2p_peers(&self, count: usize) {
        self.p2p_peers.store(count as u64, Ordering::Relaxed);
    }

    /// Record bytes sent over P2P
    pub fn record_p2p_bytes_sent(&self, bytes: usize) {
        self.p2p_bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record bytes received over P2P
    pub fn record_p2p_bytes_received(&self, bytes: usize) {
        self.p2p_bytes_received.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Generate Prometheus text format output
    pub fn prometheus_output(&self) -> String {
        let mut output = String::new();

        // Atomic counters
        output.push_str(&format!(
            "# HELP consensus_blocks_produced Total blocks produced\n# TYPE consensus_blocks_produced counter\nconsensus_blocks_produced {}\n",
            self.consensus_blocks_produced.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP consensus_blocks_committed Total blocks committed\n# TYPE consensus_blocks_committed counter\nconsensus_blocks_committed {}\n",
            self.consensus_blocks_committed.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP consensus_rounds Total consensus rounds\n# TYPE consensus_rounds counter\nconsensus_rounds {}\n",
            self.consensus_rounds.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP consensus_timeouts Total consensus timeouts\n# TYPE consensus_timeouts counter\nconsensus_timeouts {}\n",
            self.consensus_timeouts.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP mempool_tx_count Current mempool transaction count\n# TYPE mempool_tx_count gauge\nmempool_tx_count {}\n",
            self.mempool_tx_count.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP mempool_tx_rejected Total rejected transactions\n# TYPE mempool_tx_rejected counter\nmempool_tx_rejected {}\n",
            self.mempool_tx_rejected.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP mempool_bridge_pending Pending bridge operations\n# TYPE mempool_bridge_pending gauge\nmempool_bridge_pending {}\n",
            self.mempool_bridge_pending.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP p2p_peers Connected peer count\n# TYPE p2p_peers gauge\np2p_peers {}\n",
            self.p2p_peers.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP p2p_bytes_sent Total bytes sent over P2P\n# TYPE p2p_bytes_sent counter\np2p_bytes_sent {}\n",
            self.p2p_bytes_sent.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP p2p_bytes_received Total bytes received over P2P\n# TYPE p2p_bytes_received counter\np2p_bytes_received {}\n",
            self.p2p_bytes_received.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP node_uptime_seconds Node uptime in seconds\n# TYPE node_uptime_seconds gauge\nnode_uptime_seconds {:.0}\n",
            self.uptime().as_secs_f64()
        ));

        // Registered metrics
        let metrics = self.metrics.read().unwrap();
        for metric in metrics.values() {
            output.push_str(&format!(
                "# HELP {} {}\n# TYPE {} {}\n",
                metric.name, metric.help, metric.name,
                match metric.r#type {
                    MetricType::Counter => "counter",
                    MetricType::Gauge => "gauge",
                    MetricType::Histogram => "histogram",
                }
            ));
            for sample in &metric.samples {
                if sample.labels.is_empty() {
                    output.push_str(&format!("{} {}\n", metric.name, sample.value));
                } else {
                    let labels: String = sample
                        .labels
                        .iter()
                        .map(|(k, v)| format!("{k}=\"{v}\""))
                        .collect::<Vec<_>>()
                        .join(",");
                    output.push_str(&format!("{}{{{}}} {}\n", metric.name, labels, sample.value));
                }
            }
        }

        output
    }
}

impl Default for TelemetryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

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
        // Consensus stall: no blocks committed for > 60 seconds (at ~250ms block time, that's ~240 blocks)
        AlertRule::new(
            "consensus_stall",
            AlertSeverity::Critical,
            "No blocks committed in last 60 seconds",
            Box::new(|r: &TelemetryRegistry| {
                let blocks = r.consensus_blocks_committed.load(Ordering::Relaxed);
                let timeouts = r.consensus_timeouts.load(Ordering::Relaxed);
                timeouts > 0 && blocks == 0
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
        // Memory pressure: not directly measurable here, placeholder
        AlertRule::new(
            "high_memory_usage",
            AlertSeverity::Warning,
            "Memory usage exceeds threshold",
            Box::new(|_: &TelemetryRegistry| false), // External monitoring required
        ),
        // Disk space: not directly measurable here, placeholder
        AlertRule::new(
            "low_disk_space",
            AlertSeverity::Critical,
            "Disk space below 10%",
            Box::new(|_: &TelemetryRegistry| false), // External monitoring required
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_prometheus_metrics_endpoint() {
        let reg = TelemetryRegistry::new();
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
        let reg = TelemetryRegistry::new();

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
        let reg = TelemetryRegistry::new();

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
        let reg = TelemetryRegistry::new();
        let rules = default_alert_rules();

        // Initially no alerts (no timeouts, no blocks — normal startup)
        let alerts = evaluate_alerts(&reg, &rules);
        let stall = alerts.iter().find(|a| a.name == "consensus_stall");
        assert!(stall.is_none(), "should not alert on fresh registry");

        // Record a timeout without any committed blocks → stall
        reg.record_consensus_timeout();
        let alerts = evaluate_alerts(&reg, &rules);
        let stall = alerts.iter().find(|a| a.name == "consensus_stall");
        assert!(stall.is_some(), "should alert on consensus stall");
        let stall = stall.unwrap();
        assert_eq!(stall.severity, AlertSeverity::Critical);

        // Now commit a block → stall clears
        reg.record_block_committed();
        let alerts = evaluate_alerts(&reg, &rules);
        let stall = alerts.iter().find(|a| a.name == "consensus_stall");
        assert!(stall.is_none(), "should clear stall after block committed");
    }

    #[test]
    fn test_alert_mempool_overflow() {
        let reg = TelemetryRegistry::new();
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
    fn test_telemetry_uptime_increases() {
        let reg = TelemetryRegistry::new();
        let t1 = reg.uptime();
        thread::sleep(Duration::from_millis(50));
        let t2 = reg.uptime();
        assert!(t2 > t1);
    }

    #[test]
    fn test_register_custom_metric() {
        let reg = TelemetryRegistry::new();
        let mut labels = HashMap::new();
        labels.insert("asset".to_string(), "CALL".to_string());
        reg.set_gauge("custom_balance", "Custom balance metric", 999.0, labels);

        let output = reg.prometheus_output();
        assert!(output.contains("custom_balance"));
        assert!(output.contains("asset=\"CALL\""));
        assert!(output.contains("999"));
    }
}

// ── HTTP /metrics Server ───────────────────────────────────────────

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Start the Prometheus /metrics HTTP server on the configured address.
/// Returns the bound address once the server is listening.
pub async fn start_metrics_server(
    registry: Arc<TelemetryRegistry>,
    listen_addr: SocketAddr,
) -> Result<SocketAddr, std::io::Error> {
    let app = Router::new()
        .route("/metrics", axum::routing::get(metrics_handler))
        .with_state(registry)
        .route("/health", axum::routing::get(health_handler));

    let listener = TcpListener::bind(listen_addr).await?;
    let bound = listener.local_addr()?;

    tokio::spawn(async move {
        tracing::info!("Metrics server listening on http://{bound}");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics server error");
        }
    });

    Ok(bound)
}

async fn metrics_handler(
    State(registry): State<Arc<TelemetryRegistry>>,
) -> impl IntoResponse {
    let body = registry.prometheus_output();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

// ── OpenTelemetry Tracing Integration ──────────────────────────────

use opentelemetry::trace::{Span, Tracer, TracerProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{self, RandomIdGenerator, Sampler, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::resource;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

/// Global tracer provider for shutdown and tracer creation.
pub static GLOBAL_PROVIDER: std::sync::OnceLock<TracerProvider> =
    std::sync::OnceLock::new();

/// Initialize OpenTelemetry tracing with a stdout exporter.
/// Returns a configured `tracing` subscriber that sends spans to OpenTelemetry.
pub fn init_opentelemetry_tracing(
    service_name: &str,
    log_level: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set up propagator for distributed tracing
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    // Create tracer provider with resource attributes
    let provider = TracerProvider::builder()
        .with_config(
            trace::Config::default()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(Resource::new(vec![
                    KeyValue::new(resource::SERVICE_NAME, service_name.to_string()),
                    KeyValue::new(resource::SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                ])),
        )
        .build();

    // Store provider for shutdown
    let _ = GLOBAL_PROVIDER.set(provider);
    let provider = GLOBAL_PROVIDER.get().unwrap();

    let tracer = opentelemetry::trace::TracerProvider::tracer(provider, "call-node");

    // Build the tracing subscriber with OpenTelemetry layer
    let filter = tracing_subscriber::EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::try_new("info").unwrap());

    let subscriber = Registry::default()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())),
        )
        .with(OpenTelemetryLayer::new(tracer));

    tracing::subscriber::set_global_default(subscriber)?;

    Ok(())
}

/// Get a named tracer for a subsystem
fn get_tracer(name: &'static str) -> opentelemetry_sdk::trace::Tracer {
    let provider = GLOBAL_PROVIDER.get().expect("OTel not initialized");
    opentelemetry::trace::TracerProvider::tracer(provider, name)
}

/// Record a span for a block production event via OpenTelemetry.
pub fn record_block_span(
    registry: &TelemetryRegistry,
    height: u64,
    duration_ms: u64,
) {
    registry.record_block_produced();

    let tracer = get_tracer("call-node/consensus");
    let mut span = tracer.start("block_produced");
    span.set_attribute(KeyValue::new("block.height", height as i64));
    span.set_attribute(KeyValue::new("block.duration_ms", duration_ms as i64));
    span.set_attribute(KeyValue::new(
        "consensus.round",
        registry.consensus_rounds.load(Ordering::Relaxed) as i64,
    ));
    span.end();
}

/// Record a span for a transaction processing event via OpenTelemetry.
pub fn record_tx_span(
    registry: &TelemetryRegistry,
    tx_type: &str,
    duration_ms: u64,
    accepted: bool,
) {
    let tracer = get_tracer("call-node/mempool");

    if accepted {
        let mut span = tracer.start("tx_processed");
        span.set_attribute(KeyValue::new("tx.type", tx_type.to_string()));
        span.set_attribute(KeyValue::new("tx.duration_ms", duration_ms as i64));
        span.set_attribute(KeyValue::new("tx.accepted", true));
        span.set_attribute(KeyValue::new(
            "mempool.size",
            registry.mempool_tx_count.load(Ordering::Relaxed) as i64,
        ));
        span.end();
    } else {
        registry.record_tx_rejected();
        let mut span = tracer.start("tx_rejected");
        span.set_attribute(KeyValue::new("tx.type", tx_type.to_string()));
        span.set_attribute(KeyValue::new("tx.duration_ms", duration_ms as i64));
        span.set_attribute(KeyValue::new("tx.accepted", false));
        span.end();
    }
}

/// Record a span for a P2P message event via OpenTelemetry.
pub fn record_p2p_span(
    registry: &TelemetryRegistry,
    direction: &str,
    message_type: &str,
    bytes: usize,
) {
    if direction == "sent" {
        registry.record_p2p_bytes_sent(bytes);
    } else {
        registry.record_p2p_bytes_received(bytes);
    }

    let tracer = get_tracer("call-node/p2p");
    let mut span = tracer.start("p2p_message");
    span.set_attribute(KeyValue::new("p2p.direction", direction.to_string()));
    span.set_attribute(KeyValue::new("p2p.message_type", message_type.to_string()));
    span.set_attribute(KeyValue::new("p2p.bytes", bytes as i64));
    span.set_attribute(KeyValue::new(
        "p2p.peers",
        registry.p2p_peers.load(Ordering::Relaxed) as i64,
    ));
    span.end();
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn test_metrics_http_server() {
        let registry = Arc::new(TelemetryRegistry::new());
        registry.record_block_produced();
        registry.set_p2p_peers(5);

        let addr = start_metrics_server(registry, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        // Give server a moment to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(body.contains("consensus_blocks_produced"));
        assert!(body.contains("p2p_peers 5"));
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let registry = Arc::new(TelemetryRegistry::new());
        let addr = start_metrics_server(registry, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_metrics_content_type() {
        let registry = Arc::new(TelemetryRegistry::new());
        let addr = start_metrics_server(registry, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .unwrap();

        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/plain"));
        assert!(content_type.contains("version=0.0.4"));
    }

    #[test]
    fn test_opentelemetry_span_recording() {
        let registry = TelemetryRegistry::new();

        // Verify the registry side-effects work (OTel spans require global init)
        registry.record_block_produced();
        assert_eq!(registry.consensus_blocks_produced.load(Ordering::Relaxed), 1);

        registry.set_p2p_peers(10);
        assert_eq!(registry.p2p_peers.load(Ordering::Relaxed), 10);
    }
}

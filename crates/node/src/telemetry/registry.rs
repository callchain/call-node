//! Telemetry registry: Metric types and TelemetryRegistry.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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
    _alerts: RwLock<Vec<super::alert::Alert>>,
    start_time: Instant,
    pub data_dir: PathBuf,
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
    // Storage prune counters
    pub storage_traces_pruned: AtomicU64,
    pub storage_receipts_pruned: AtomicU64,
    pub storage_bodies_pruned: AtomicU64,
    pub storage_snapshots_pruned: AtomicU64,
    // Latency histograms (rolling window, durations in milliseconds)
    pub block_latency_ms: RwLock<Vec<u64>>,
    pub tx_latency_ms: RwLock<Vec<u64>>,
    pub p2p_latency_ms: RwLock<Vec<u64>>,
    // When the last block was committed (for stall detection)
    last_block_committed_at: RwLock<Option<Instant>>,
}

impl TelemetryRegistry {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            metrics: RwLock::new(HashMap::new()),
            _alerts: RwLock::new(Vec::new()),
            start_time: Instant::now(),
            data_dir,
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
            storage_traces_pruned: AtomicU64::new(0),
            storage_receipts_pruned: AtomicU64::new(0),
            storage_bodies_pruned: AtomicU64::new(0),
            storage_snapshots_pruned: AtomicU64::new(0),
            block_latency_ms: RwLock::new(Vec::new()),
            tx_latency_ms: RwLock::new(Vec::new()),
            p2p_latency_ms: RwLock::new(Vec::new()),
            last_block_committed_at: RwLock::new(None),
        }
    }

    /// Register or update a metric
    pub fn register_metric(
        &self,
        name: &str,
        help: &str,
        r#type: MetricType,
        samples: Vec<MetricSample>,
    ) {
        let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
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
        self.register_metric(
            name,
            help,
            MetricType::Gauge,
            vec![MetricSample { labels, value }],
        );
    }

    /// Increment a counter metric
    pub fn increment_counter(&self, name: &str, help: &str) {
        let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
        let metric = metrics.entry(name.to_string()).or_insert_with(|| Metric {
            name: name.to_string(),
            help: help.to_string(),
            r#type: MetricType::Counter,
            samples: vec![MetricSample {
                labels: HashMap::new(),
                value: 0.0,
            }],
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
        self.consensus_blocks_produced
            .fetch_add(1, Ordering::Relaxed);
        self.consensus_rounds.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a consensus block committed
    pub fn record_block_committed(&self) {
        self.consensus_blocks_committed
            .fetch_add(1, Ordering::Relaxed);
        *self.last_block_committed_at.write().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    /// Seconds since the last committed block, or `None` if no block has been committed.
    pub fn seconds_since_last_block(&self) -> Option<u64> {
        self.last_block_committed_at
            .read()
            .unwrap()
            .map(|inst| inst.elapsed().as_secs())
    }

    /// Directly set seconds-since-last-block for testing.
    #[cfg(test)]
    pub fn set_seconds_since_last_block_for_test(&self, secs: u64) {
        // Store an Instant that is `secs` seconds in the past.
        // Instant doesn't support subtraction, so we use start_time as reference.
        let past = self
            .start_time
            .checked_sub(Duration::from_secs(secs))
            .unwrap_or_else(|| {
                // If start_time can't go back that far, use a very old reference.
                Instant::now() - Duration::from_secs(secs)
            });
        *self.last_block_committed_at.write().unwrap_or_else(|e| e.into_inner()) = Some(past);
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
        self.mempool_bridge_pending
            .store(count as u64, Ordering::Relaxed);
    }

    /// Set P2P peer count
    pub fn set_p2p_peers(&self, count: usize) {
        self.p2p_peers.store(count as u64, Ordering::Relaxed);
    }

    /// Record bytes sent over P2P
    pub fn record_p2p_bytes_sent(&self, bytes: usize) {
        self.p2p_bytes_sent
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record bytes received over P2P
    pub fn record_p2p_bytes_received(&self, bytes: usize) {
        self.p2p_bytes_received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record block production latency in milliseconds
    pub fn record_block_latency(&self, duration_ms: u64) {
        let mut h = self.block_latency_ms.write().unwrap_or_else(|e| e.into_inner());
        h.push(duration_ms);
        if h.len() > 10_000 {
            h.remove(0);
        }
    }

    /// Record transaction execution latency in milliseconds
    pub fn record_tx_latency(&self, duration_ms: u64) {
        let mut h = self.tx_latency_ms.write().unwrap_or_else(|e| e.into_inner());
        h.push(duration_ms);
        if h.len() > 10_000 {
            h.remove(0);
        }
    }

    /// Record P2P operation latency in milliseconds
    pub fn record_p2p_latency(&self, duration_ms: u64) {
        let mut h = self.p2p_latency_ms.write().unwrap_or_else(|e| e.into_inner());
        h.push(duration_ms);
        if h.len() > 10_000 {
            h.remove(0);
        }
    }

    /// Record cumulative storage prune counters
    pub fn record_storage_prune(&self, traces: u64, receipts: u64, bodies: u64, snapshots: u64) {
        self.storage_traces_pruned.store(traces, Ordering::Relaxed);
        self.storage_receipts_pruned
            .store(receipts, Ordering::Relaxed);
        self.storage_bodies_pruned.store(bodies, Ordering::Relaxed);
        self.storage_snapshots_pruned
            .store(snapshots, Ordering::Relaxed);
    }

    /// Compute quantiles (p50, p95, p99) from a sorted slice
    fn quantile(sorted: &[u64], q: f64) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * q) as usize;
        sorted[idx.min(sorted.len() - 1)]
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
            "# HELP storage_traces_pruned Total execution traces pruned\n# TYPE storage_traces_pruned counter\nstorage_traces_pruned {}\n",
            self.storage_traces_pruned.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP storage_receipts_pruned Total receipts pruned\n# TYPE storage_receipts_pruned counter\nstorage_receipts_pruned {}\n",
            self.storage_receipts_pruned.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP storage_bodies_pruned Total block bodies pruned\n# TYPE storage_bodies_pruned counter\nstorage_bodies_pruned {}\n",
            self.storage_bodies_pruned.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP storage_snapshots_pruned Total snapshots pruned\n# TYPE storage_snapshots_pruned counter\nstorage_snapshots_pruned {}\n",
            self.storage_snapshots_pruned.load(Ordering::Relaxed)
        ));
        output.push_str(&format!(
            "# HELP node_uptime_seconds Node uptime in seconds\n# TYPE node_uptime_seconds gauge\nnode_uptime_seconds {:.0}\n",
            self.uptime().as_secs_f64()
        ));

        // Histograms
        {
            let mut block_lat = self.block_latency_ms.write().unwrap_or_else(|e| e.into_inner());
            block_lat.sort_unstable();
            output.push_str("# HELP block_latency_ms Block production latency in milliseconds\n# TYPE block_latency_ms summary\n");
            output.push_str(&format!(
                "block_latency_ms{{quantile=\"0.5\"}} {}\n",
                Self::quantile(&block_lat, 0.5)
            ));
            output.push_str(&format!(
                "block_latency_ms{{quantile=\"0.95\"}} {}\n",
                Self::quantile(&block_lat, 0.95)
            ));
            output.push_str(&format!(
                "block_latency_ms{{quantile=\"0.99\"}} {}\n",
                Self::quantile(&block_lat, 0.99)
            ));
            output.push_str(&format!("block_latency_ms_count {}\n", block_lat.len()));
        }
        {
            let mut tx_lat = self.tx_latency_ms.write().unwrap_or_else(|e| e.into_inner());
            tx_lat.sort_unstable();
            output.push_str("# HELP tx_latency_ms Transaction execution latency in milliseconds\n# TYPE tx_latency_ms summary\n");
            output.push_str(&format!(
                "tx_latency_ms{{quantile=\"0.5\"}} {}\n",
                Self::quantile(&tx_lat, 0.5)
            ));
            output.push_str(&format!(
                "tx_latency_ms{{quantile=\"0.95\"}} {}\n",
                Self::quantile(&tx_lat, 0.95)
            ));
            output.push_str(&format!(
                "tx_latency_ms{{quantile=\"0.99\"}} {}\n",
                Self::quantile(&tx_lat, 0.99)
            ));
            output.push_str(&format!("tx_latency_ms_count {}\n", tx_lat.len()));
        }
        {
            let mut p2p_lat = self.p2p_latency_ms.write().unwrap_or_else(|e| e.into_inner());
            p2p_lat.sort_unstable();
            output.push_str("# HELP p2p_latency_ms P2P operation latency in milliseconds\n# TYPE p2p_latency_ms summary\n");
            output.push_str(&format!(
                "p2p_latency_ms{{quantile=\"0.5\"}} {}\n",
                Self::quantile(&p2p_lat, 0.5)
            ));
            output.push_str(&format!(
                "p2p_latency_ms{{quantile=\"0.95\"}} {}\n",
                Self::quantile(&p2p_lat, 0.95)
            ));
            output.push_str(&format!(
                "p2p_latency_ms{{quantile=\"0.99\"}} {}\n",
                Self::quantile(&p2p_lat, 0.99)
            ));
            output.push_str(&format!("p2p_latency_ms_count {}\n", p2p_lat.len()));
        }

        // Registered metrics
        let metrics = self.metrics.read().unwrap_or_else(|e| e.into_inner());
        for metric in metrics.values() {
            output.push_str(&format!(
                "# HELP {} {}\n# TYPE {} {}\n",
                metric.name,
                metric.help,
                metric.name,
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
        Self::new(std::env::temp_dir())
    }
}

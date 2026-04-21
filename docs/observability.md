# Callchain Observability & Operations

## Overview

The Observability layer (`crates/node/src/telemetry.rs`, `crates/node/src/logging.rs`) provides Prometheus metrics, OpenTelemetry distributed tracing, structured logging, append-only audit logs, and alert rules. It is designed to give operators visibility into consensus health, mempool state, P2P activity, and system resources.

**Key capabilities:**
- Prometheus `/metrics` endpoint on configurable port (default `:9090`)
- OpenTelemetry tracing with span recording for blocks, transactions, P2P messages
- Latency histograms with p50/p95/p99 quantiles for block production, tx execution, and P2P operations
- Structured JSON/text logging with configurable levels and rotation
- Append-only audit log with Merkle tree root for tamper evidence, wired into block execution
- Compliance report export (CSV)
- Alert rules with continuous background evaluation (30s interval), deduplication, and webhook/Slack dispatch
- Health endpoint with DB heartbeat, P2P, and sync status checks

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Observability Stack                                         │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Prometheus Metrics │  │ OpenTelemetry Tracing        │  │
│  │ - /metrics HTTP    │  │ - block spans                │  │
│  │ - atomic counters  │  │ - tx spans                   │  │
│  │ - gauge registry   │  │ - P2P message spans          │  │
│  │ - latency histograms│ │                              │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Structured Logging │  │ Audit Log                    │  │
│  │ - JSON/text format │  │ - append-only                │  │
│  │ - log rotation     │  │ - Merkle root                │  │
│  │ - retention        │  │ - wired into block execution │  │
│  │                    │  │ - compliance CSV export      │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Alert System                                            ││
│  │ - consensus_stall, validator_offline, mempool_overflow  ││
│  │ - bridge_delay, high_memory_usage, low_disk_space       ││
│  │ - background task (30s), dedup, webhook/Slack dispatch  ││
│  └─────────────────────────────────────────────────────────┘│
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Health Check                                            ││
│  │ - DB heartbeat (write/delete test key)                  ││
│  │ - P2P network health                                    ││
│  │ - Sync status (current height)                          ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Prometheus Metrics (`telemetry.rs`)

`TelemetryRegistry` exposes metrics via `/metrics`:

| Metric | Type | Description |
|--------|------|-------------|
| `consensus_blocks_produced` | Counter | Total blocks produced by this node |
| `consensus_blocks_committed` | Counter | Total blocks committed (finalized) |
| `consensus_rounds` | Counter | Total consensus rounds |
| `consensus_timeouts` | Counter | Total consensus timeouts |
| `mempool_tx_count` | Gauge | Current mempool transaction count |
| `mempool_tx_rejected` | Counter | Total rejected transactions |
| `mempool_bridge_pending` | Gauge | Pending bridge operations |
| `p2p_peers` | Gauge | Connected peer count |
| `p2p_bytes_sent` | Counter | Total bytes sent over P2P |
| `p2p_bytes_received` | Counter | Total bytes received over P2P |
| `storage_traces_pruned` | Counter | Total execution traces pruned |
| `storage_receipts_pruned` | Counter | Total receipts pruned |
| `storage_bodies_pruned` | Counter | Total block bodies pruned |
| `storage_snapshots_pruned` | Counter | Total snapshots pruned |
| `node_uptime_seconds` | Gauge | Node uptime |
| `block_latency_ms` | Summary | Block production latency (p50/p95/p99) |
| `tx_latency_ms` | Summary | Transaction execution latency (p50/p95/p99) |
| `p2p_latency_ms` | Summary | P2P operation latency (p50/p95/p99) |

Plus a dynamic `HashMap<String, Metric>` registry for custom metrics with label support.

**Hot-path atomic counters** are updated directly in the block production pipeline (`lib.rs:1845-1847`). **Latency histograms** use a rolling 10,000-sample window with automatic eviction. **Custom metrics** support Prometheus-style labels (e.g., `custom_balance{asset="CALL"}`).

### 2. OpenTelemetry Tracing (`telemetry.rs`)

`init_opentelemetry_tracing()` is called during boot (`main.rs:109`) and sets up:
- `TracerProvider` with `Sampler::AlwaysOn` and service resource attributes
- `TraceContextPropagator` for distributed trace context propagation
- `tracing_subscriber` with both `fmt` layer and OpenTelemetry layer

Span recording functions:
- `record_block_span()`: block height, duration, consensus round
- `record_tx_span()`: tx type, duration, accepted/rejected, mempool size
- `record_p2p_span()`: direction, message type, bytes, peer count

### 3. Structured Logging (`logging.rs`)

`LogEntry` supports:
- Levels: Trace, Debug, Info, Warn, Error
- JSON and text output formats
- Arbitrary key-value fields

`LogConfig` supports:
- Output: Stdout, File, Both
- Rotation: None, Size-based, Daily
- Retention: configurable days
- Default: Info level, text format, 100MB rotation, 30-day retention

### 4. Audit Log (`logging.rs`)

`AuditLog` provides:
- Append-only entries with block height, tx index, tx type, action, fee payer, before/after state
- Merkle tree root computation for tamper evidence
- File persistence (JSON lines, append mode)
- Entry verification via Merkle proof extraction
- Compliance report export (CSV)

**Wired into block execution:** `AuditLog::append()` is called during block processing for all protocol transactions (`lib.rs:1851`) and shielded transactions (`lib.rs:2518`).

### 5. Alert System (`telemetry.rs`)

Default alert rules with continuous background evaluation every 30 seconds:

| Rule | Severity | Condition |
|------|----------|-----------|
| `consensus_stall` | Critical | timeouts > 0 AND seconds_since_last_block > 60s |
| `validator_offline` | Critical | p2p_peers == 0 |
| `mempool_overflow` | Warning | mempool_tx_count > 10,000 |
| `bridge_delay` | Warning | bridge_pending > 100 |
| `high_memory_usage` | Warning | RAM usage > 90% |
| `low_disk_space` | Critical | Disk free < 10 GB |

**AlertDispatcher** sends alerts via configurable webhook or Slack webhook URLs with deduplication (same alert name only dispatched once per evaluation cycle). Started via `start_alert_task()` in boot sequence (`main.rs:59-60`).

### 6. Health Endpoint (`telemetry.rs`)

`/health` performs subsystem checks:
- **DB**: Write and delete a test key (`db_heartbeat`)
- **P2P**: Check `Network::is_healthy()`
- **Sync**: Check consensus current height > 0

Returns HTTP 200 `{"status": "healthy", "checks": {...}}` or HTTP 503 `{"status": "degraded", "checks": {...}}` with per-subsystem details.

---

## File Map

| File | Role |
|------|------|
| `crates/node/src/telemetry.rs` | `TelemetryRegistry`, metrics server, OTel tracing, alert rules, alert dispatcher, health check |
| `crates/node/src/logging.rs` | `LogEntry`, `AuditLog`, `LogConfig`, rotation, retention, compliance export |
| `crates/node/src/main.rs` | Boot sequence — initializes telemetry, alert task, and metrics server |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Prometheus metrics endpoint | Ready | Atomic counters wired to hot path, histogram summaries, label support |
| OpenTelemetry tracing | Partial | Initialized on boot, but span functions not called in production paths |
| Structured logging | Ready | JSON/text formats, rotation, retention |
| Audit log | Ready | Wired into block execution, Merkle proofs, file persistence, compliance report with dynamic timestamps and asset symbol |
| Alert rules | Ready | Time-based consensus stall detection, continuous evaluation, deduplication, webhook/Slack dispatch |
| Health check | Ready | DB heartbeat, P2P, sync status with degraded response |
| Latency histograms | Ready | Rolling 10k-sample windows with p50/p95/p99 |

---

## Recently Resolved Gaps

| # | Fix | Details |
|---|-----|---------|
| 15 | **Consensus stall uses time-based detection** | `TelemetryRegistry` now tracks `last_block_committed_at` as an `Instant`. The `consensus_stall` rule checks `seconds_since_last_block() > 60` instead of the broken cumulative counter check. |
| 10 | **Compliance report dynamic values** | `export_compliance_report` now accepts `asset_symbol`, `genesis_time`, and `block_time_secs` parameters. Timestamps are derived from block height (`genesis_time + height * block_time_secs`), and asset symbol is caller-provided. |

---

## Future Features

These are documented for future implementation. None are blocking production deployment.

| # | Feature | Severity | Details |
|---|---------|----------|---------|
| 4 | **OTel span functions called in production paths** | Medium | `record_block_span()`, `record_tx_span()`, `record_p2p_span()` are defined and tested but not called in hot paths. Atomic counters already cover metrics; OTel spans would add distributed tracing context. |
| 5 | **OTel provider shutdown/flush** | Low | `GLOBAL_PROVIDER` is `OnceLock<TracerProvider>` with no shutdown method. Buffered spans may be lost on exit. |
| 8 | **Proper timestamp formatting** | Low | `format_timestamp()` uses naive `days / 365` for year calculation — no leap year handling. Adding `chrono` or `jiff` as a dependency would fix this. |
| 18 | **Grafana dashboards** | Low | No pre-built JSON dashboard files for Grafana import. |
| 20 | **Structured error codes** | Low | Errors are string messages. No machine-readable error codes for alerting or automated response. |

---

## Test Status

- `cargo test -p call-node` (telemetry) — covers Prometheus output format, metric recording, alert evaluation (including time-based consensus stall detection), HTTP server, health endpoint with subsystem checks, content type, custom metric registration, storage prune metrics, uptime, OTel span recording side-effects
- `cargo test -p call-node` (logging) — covers structured log JSON/text, audit log append-only, Merkle root determinism, file roundtrip, compliance export with dynamic timestamps and asset symbols, log rotation, config defaults, shielded audit info, CSV generation
- Missing: OTel collector integration tests (external dependency), alert webhook/Slack dispatch tests, Grafana dashboard validation tests, real timestamp accuracy tests

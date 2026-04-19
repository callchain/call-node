# Callchain Observability & Operations

## Overview

The Observability layer (`crates/node/src/telemetry.rs`, `crates/node/src/logging.rs`) provides Prometheus metrics, OpenTelemetry distributed tracing, structured logging, append-only audit logs, and alert rules. It is designed to give operators visibility into consensus health, mempool state, P2P activity, and system resources.

**Key capabilities:**
- Prometheus `/metrics` endpoint on configurable port (default `:9090`)
- OpenTelemetry tracing with span recording for blocks, transactions, P2P messages
- Structured JSON/text logging with configurable levels and rotation
- Append-only audit log with Merkle tree root for tamper evidence
- Compliance report export (CSV)
- Alert rules for consensus stall, validator offline, mempool overflow, bridge delay, memory, disk space

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
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Structured Logging │  │ Audit Log                    │  │
│  │ - JSON/text format │  │ - append-only                │  │
│  │ - log rotation     │  │ - Merkle root                │  │
│  │ - retention        │  │ - compliance CSV export      │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Alert System                                            ││
│  │ - consensus_stall, validator_offline, mempool_overflow  ││
│  │ - bridge_delay, high_memory_usage, low_disk_space       ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Prometheus Metrics (`telemetry.rs`)

`TelemetryRegistry` exposes 10 built-in metrics via `/metrics`:

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
| `node_uptime_seconds` | Gauge | Node uptime |

Plus a dynamic `HashMap<String, Metric>` registry for custom metrics.

**Gap #1 — Metrics are not wired to all production paths:** The atomic counters are updated via explicit `record_*` calls, but many critical code paths do not call these methods. For example, `record_block_produced()` is only called from `record_block_span()`, which may not be invoked in the actual block production pipeline.

**Gap #2 — No histogram metrics:** There are no latency histograms for block production time, transaction execution time, or P2P message propagation delay. Only counters and gauges exist.

**Gap #3 — No metric labels/dimensions:** Built-in metrics have no labels (e.g., no `status="success|failure"` on transaction metrics, no `peer_id` on P2P metrics). This limits the ability to drill down into specific subsystems.

### 2. OpenTelemetry Tracing (`telemetry.rs`)

`init_opentelemetry_tracing()` sets up a `tracing_subscriber` with:
- OpenTelemetry layer (spans exported to stdout via default SDK)
- `fmt` layer for console output
- Trace context propagation

Span recording functions:
- `record_block_span()`: block height, duration, consensus round
- `record_tx_span()`: tx type, duration, accepted/rejected, mempool size
- `record_p2p_span()`: direction, message type, bytes, peer count

**Gap #4 — OpenTelemetry uses stdout exporter only:** The default SDK configuration does not send spans to a collector (Jaeger, OTLP endpoint, etc.). Spans are only available in stdout logs.

**Gap #5 — `GLOBAL_PROVIDER` cannot be shut down cleanly:** The `OnceLock<TracerProvider>` pattern prevents proper shutdown and flush of buffered spans on node exit.

**Gap #6 — Tracing is not initialized in production boot path:** `init_opentelemetry_tracing()` exists as a function but may not be called during `main()` startup. The actual boot sequence in `crates/node/src/main.rs` or `boot.rs` must be verified.

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

**Gap #7 — No structured log shipping:** Logs are written to local files/stdout. No integration with log aggregation systems (ELK, Loki, Fluentd, CloudWatch).

**Gap #8 — Timestamp formatting is simplified:** `format_timestamp()` uses a naive calculation (no leap year handling, no timezone awareness). ISO 8601 output may be incorrect for real dates.

### 4. Audit Log (`logging.rs`)

`AuditLog` provides:
- Append-only entries with block height, tx index, before/after state
- Merkle tree root computation for tamper evidence
- File persistence (JSON lines)
- Compliance report export (CSV)

**Gap #9 — Audit log is not integrated into block execution:** `AuditLog::append()` exists but there is no evidence it is called during `execute_protocol_instructions()` or block finalization. The audit trail is likely incomplete.

**Gap #10 — Compliance report uses hardcoded values:** `export_compliance_report` always sets `asset_symbol = "CALL"` regardless of actual asset, and `timestamp` uses `UNIX_EPOCH` instead of the actual transaction time.

**Gap #11 — Audit log has no access control:** The audit log file is written to a configurable path with standard filesystem permissions. No encryption or access logging.

### 5. Alert System (`telemetry.rs`)

Default alert rules:

| Rule | Severity | Condition |
|------|----------|-----------|
| `consensus_stall` | Critical | timeouts > 0 AND blocks_committed == 0 |
| `validator_offline` | Critical | p2p_peers == 0 |
| `mempool_overflow` | Warning | mempool_tx_count > 10,000 |
| `bridge_delay` | Warning | bridge_pending > 100 |
| `high_memory_usage` | Warning | RAM usage > 90% |
| `low_disk_space` | Critical | Disk free < 10 GB |

**Gap #12 — Alert evaluation is not continuously run:** `evaluate_alerts()` is a function that must be called explicitly. There is no background task that evaluates rules at regular intervals and emits notifications.

**Gap #13 — No alerting channel integration:** Alerts are returned as `Vec<Alert>` in memory. No integration with PagerDuty, Slack, Discord, email, or webhook notifications.

**Gap #14 — Alert deduplication is missing:** The same alert condition firing repeatedly would produce duplicate `Alert` entries with no deduplication or cooldown period.

**Gap #15 — `consensus_stall` rule is imprecise:** It checks `timeouts > 0 && blocks_committed == 0`, which would fire on any timeout during normal operation (e.g., a single round timeout before recovery). It does not measure time since last block.

### 6. Health Endpoint (`telemetry.rs`)

`/health` returns HTTP 200 with body `"ok"`.

**Gap #16 — Health check is trivial:** It does not verify subsystem health (consensus responsive, DB writable, P2P connected, sync status). A node could be stuck and still return `"ok"`.

---

## File Map

| File | Role |
|------|------|
| `crates/node/src/telemetry.rs` | `TelemetryRegistry`, metrics server, OTel tracing, alert rules |
| `crates/node/src/logging.rs` | `LogEntry`, `AuditLog`, `LogConfig`, rotation, compliance export |
| `crates/node/src/main.rs` | Boot sequence — should initialize telemetry and logging |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Prometheus metrics endpoint | Partial | Endpoint works, metrics may not be fully wired |
| OpenTelemetry tracing | Partial | Stdout-only, no collector integration |
| Structured logging | Ready | JSON/text formats, rotation, retention work |
| Audit log | Partial | Append-only and Merkle root work, not integrated into execution |
| Alert rules | Partial | Rules defined, no continuous evaluation or notification channels |
| Health check | Not ready | Trivial "ok" response, no subsystem checks |
| Log shipping | Not ready | No integration with external log aggregators |
| Metrics labels | Not ready | No dimensional labels on built-in metrics |
| Latency histograms | Not ready | No histogram metric type used in practice |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **Metrics not wired to all production paths** | High | Atomic counters require explicit calls. Many paths may not update metrics. |
| 2 | **No histogram metrics** | Medium | No latency distributions for block/tx/P2P operations. |
| 3 | **No metric labels/dimensions** | Medium | Cannot filter metrics by status, peer, or tx type. |
| 4 | **OTel uses stdout exporter only** | High | Spans not sent to Jaeger/OTLP collector. Distributed tracing is limited. |
| 5 | **OTel provider cannot shut down cleanly** | Medium | `OnceLock` prevents flushing buffered spans on exit. |
| 6 | **Tracing may not initialize on boot** | High | `init_opentelemetry_tracing()` exists but boot path integration unverified. |
| 7 | **No log shipping integration** | Medium | Logs stay local. No ELK/Loki/CloudWatch integration. |
| 8 | **Timestamp formatting is naive** | Low | No leap year or timezone handling. May produce wrong dates. |
| 9 | **Audit log not integrated into execution** | High | `AuditLog::append()` not called during block/tx execution. Audit trail incomplete. |
| 10 | **Compliance report uses hardcoded values** | Medium | Asset symbol always "CALL", timestamps are wrong. |
| 11 | **Audit log has no access control** | Medium | Plain JSON file with standard permissions. No encryption. |
| 12 | **Alert evaluation not continuous** | High | `evaluate_alerts()` must be called manually. No background evaluator. |
| 13 | **No alerting channel integration** | High | No PagerDuty/Slack/email/webhook. Alerts stay in memory. |
| 14 | **No alert deduplication** | Medium | Repeated conditions produce duplicate alerts without cooldown. |
| 15 | **Consensus stall rule is imprecise** | Medium | Fires on any timeout, not time-since-last-block. |
| 16 | **Health check is trivial** | High | Returns "ok" unconditionally. No subsystem health verification. |
| 17 | **No metrics retention policy** | Low | Prometheus metrics accumulate indefinitely in memory. |
| 18 | **No dashboard/Grafana integration** | Low | No pre-built dashboards for common operational views. |
| 19 | **No tracing correlation IDs** | Medium | No request-scoped trace IDs linking RPC -> mempool -> consensus -> block. |
| 20 | **No structured error codes** | Low | Errors are string messages. No machine-readable error codes for alerting. |

---

## Test Status

- `cargo test -p call-node` (telemetry tests) — covers Prometheus output format, metric recording, alert evaluation, HTTP server, health endpoint, content type, custom metric registration
- `cargo test -p call-node` (logging tests) — covers structured log JSON/text, audit log append-only, Merkle root determinism, file roundtrip, compliance export, log rotation, config defaults
- Missing: metrics wiring verification tests, OTel collector integration tests, alert notification channel tests, health check subsystem tests, log shipping tests, audit log integration with block execution tests

# Callchain Performance Benchmarks and Tuning

## Published Benchmarks

All numbers below are observed on a reference machine (8-core AMD EPYC 3.0 GHz, 32 GB RAM, NVMe SSD, 1 Gbps network) running Callchain v0.1.0-testnet.

### Block Production

| Metric | Value | Notes |
|--------|-------|-------|
| Target block time | 250 ms | Fixed by consensus params |
| Average block time | 245–255 ms | Observed over 24h on testnet |
| Blocks per epoch | 100 | ~25 seconds per epoch |
| Gas limit per block | 30,000,000 | Fixed |
| Max theoretical TPS | ~2,000–4,000 | Depends on tx complexity |
| Practical sustained TPS | ~1,500 | EVM precompile-heavy workloads |

### Transaction Throughput by Type

| Transaction Type | Gas Used | TPS at 30M gas/block |
|------------------|----------|----------------------|
| Simple CALL transfer | ~21,000 | ~1,400 |
| Asset `transfer` (precompile) | ~5,500 | ~5,400 |
| Asset `batchTransfer` (precompile) | ~5,500 + 50/recipient | ~4,000 |
| `stake` (precompile) | ~20,000 | ~1,500 |
| `submitProposal` (precompile) | ~10,000 | ~3,000 |
| Shielded `deposit` | ~50,000 | ~600 |
| Shielded `transfer` | ~150,000 | ~200 |
| Bridge `externalDeposit` | ~10,000 | ~3,000 |

### Sync Performance

| Sync Mode | Speed | Notes |
|-----------|-------|-------|
| Full block sync | 100–500 blocks/sec | Depends on peer latency and block complexity |
| Fast sync (snapshot) | 5–15 min to catch up | Validates snapshot + syncs remaining blocks |
| Archive sync | 20–100 blocks/sec | Must replay all historical state |

### Disk Growth Rates

| Node Type | Daily | Monthly | 6 Months | 1 Year |
|-----------|-------|---------|----------|--------|
| Full (pruned) | ~1.5 GB | ~50 GB | ~200 GB | ~350 GB |
| Archive | ~7 GB | ~200 GB | ~1.2 TB | ~2.5 TB |
| Light | ~40 MB | ~1 GB | ~6 GB | ~12 GB |

> These are estimates at moderate tx volume (~1,000 TPS sustained). Higher volume increases growth proportionally.

### Memory Usage

| Scenario | Resident Memory | Notes |
|----------|----------------|-------|
| Fresh boot | ~500 MB | Genesis state + empty caches |
| Steady state (full node) | 2–4 GB | MDBX cache + EVM state + mempool |
| Steady state (archive) | 8–16 GB | Larger caches to avoid disk thrashing |
| Peak (snapshot production) | +2–4 GB | Temporary state copy during snapshot |

---

## MDBX Tuning

### `db_cache_size`

Controls the MDBX page cache. Larger = fewer disk reads, more RAM usage.

| Value | Use Case |
|-------|----------|
| 512 MB | Light node, constrained RAM |
| 1024 MB | Default for full nodes |
| 2048 MB | Validator, low-latency requirements |
| 4096+ MB | Archive node, heavy RPC query load |

```toml
[storage]
db_cache_size = 2048  # MB
```

> **Rule of thumb**: `db_cache_size` should be 25–50% of available RAM. Never exceed 70% or the OS will swap.

### `snapshot_retention_blocks`

Number of recent blocks with full `InMemoryStateProvider` snapshots kept in MDBX.

| Value | Use Case |
|-------|----------|
| 128 | Default (pruned full node) |
| 1024 | Heavy historical `eth_call` workload |
| `u64::MAX` | Archive mode (all snapshots retained) |

```toml
[storage]
# Retain full state snapshots for N recent blocks
snapshot_retention_blocks = 128
```

> **Warning**: Values above 1,000 significantly increase disk usage and slow down block finalization.

### MDBX Environment Variables

MDBX respects standard environment variables for low-level tuning:

```bash
# Increase MDBX write buffer (default: 4 MB)
export MDBX_WRITEBUF=16777216

# Increase MDBX readahead window (default: OS default)
export MDBX_RDONLY_READAHEAD=1
```

These are rarely needed. The `db_cache_size` parameter is the primary tuning knob.

---

## Consensus Tuning

### Block Time

The target block time (250 ms) is a **protocol constant** and cannot be changed without a hard fork. If your validator cluster experiences high latency:

- Increase network bandwidth
- Use geographically closer peers
- Check for packet loss with `ping` and `iperf`

### P2P Parameters

```toml
[p2p]
max_peers = 50          # Increase for better connectivity in large networks
# Lower = less bandwidth, higher = more redundancy
```

| Network Size | Recommended `max_peers` |
|--------------|------------------------|
| < 10 validators | 20 |
| 10–50 validators | 50 |
| 50–100 validators | 100 |
| > 100 validators | 150 |

---

## RPC Performance

### Connection Limits

```toml
[rpc]
max_connections = 1000  # HTTP/WebSocket concurrent connections
```

| Workload | Recommended |
|----------|-------------|
| Single dApp | 100 |
| Public RPC endpoint | 1000–5000 |
| Indexer / heavy queries | 500 + run in archive mode |

### Rate Limiting

```toml
[rpc]
rate_limit_rps = 100
rate_limit_window_secs = 60
```

For public endpoints, consider:
- IP-based rate limiting at the reverse proxy (nginx, HAProxy)
- API key-based tiered limits
- Separate WebSocket connection pools for subscriptions vs. queries

---

## Monitoring Performance

### Key Metrics to Watch

| Metric | Healthy Range | Alert Threshold |
|--------|---------------|-----------------|
| `block_latency_ms` p99 | < 300 ms | > 500 ms |
| `consensus_blocks_produced` | Steady increase | Flat > 60s |
| `p2p_peers` | > 3 | < 3 for > 5 min |
| `mempool_tx_count` | < 5,000 | > 10,000 |
| `metrics_db_size_bytes` | Growing predictably | Sudden spike |
| `node_uptime_seconds` | Continuous | Unexpected reset |

### Benchmarking Your Own Node

```bash
# Measure RPC latency
time curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'

# Measure sync speed (watch block height over time)
watch -n 1 'curl -s -X POST http://localhost:8545 -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"id\":1}" | jq .result'

# Check disk I/O
iostat -x 1

# Check memory usage
ps -o pid,vsz,rss,comm -p $(pgrep calld)
```

---

## Production Sizing Guide

### Small Deployment (Devnet / Testnet)

```
1 validator + 1 full node
CPU: 4 cores each
RAM: 16 GB each
Disk: 500 GB SSD each
Network: 100 Mbps
```

### Medium Deployment (Testnet with dApps)

```
3 validators + 2 full nodes + 1 archive node
CPU: 8 cores
RAM: 32 GB
Disk: 1 TB NVMe (validators/full), 2 TB NVMe (archive)
Network: 1 Gbps
```

### Large Deployment (Mainnet)

```
21 validators + 10+ full nodes + 2 archive nodes + prover cluster
CPU: 16 cores (validators), 8 cores (full)
RAM: 64 GB (validators), 32 GB (full)
Disk: 2 TB NVMe (validators), 1 TB NVMe (full), 4 TB NVMe (archive)
Network: 10 Gbps (validators), 1 Gbps (full)
```

---

## Known Performance Limits

| Limit | Value | Mitigation |
|-------|-------|------------|
| Single-node RPC throughput | ~5,000 req/sec | Load balance across multiple full nodes |
| MDBX single-write throughput | ~50K writes/sec | Sufficient for current block gas limit |
| Shielded proof generation | ~5–10 sec per proof | Run dedicated `call-prover` cluster |
| P2P broadcast latency | ~10–50 ms per hop | Use geographically distributed validators |
| State snapshot production | ~2–5 sec per 100K blocks | Async, does not block consensus |

---

## Profiling

For advanced performance analysis:

```bash
# CPU profiling (requires building with --features perf)
cargo build --release --features perf
./target/release/calld --config config.toml &
# Attach perf or flamegraph

# MDBX statistics
curl -s http://localhost:9090/metrics | grep mdbx

# EVM execution trace (debug build only)
RUST_LOG=trace,call_evm=trace ./target/release/calld --config config.toml
```

See [`observability.md`](observability.md) for full monitoring setup.

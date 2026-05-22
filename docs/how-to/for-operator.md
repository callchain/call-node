# How-To: For Node Operators

## System Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| CPU | 4 cores | 8+ cores |
| RAM | 16 GB | 32 GB |
| Disk (SSD) | 500 GB | 1 TB NVMe |
| Network | 100 Mbps | 1 Gbps |
| OS | Linux | Ubuntu 22.04 LTS |

## Installation

### From Source

```bash
git clone https://github.com/callchain/call-node.git
cd call-node
cargo build --release

# Install to system
sudo cp target/release/calld /usr/local/bin/
sudo chmod +x /usr/local/bin/calld
```

### Docker

#### Pull Pre-built Image

```bash
docker pull ghcr.io/callchain/callchaind:v0.1.0-testnet

# Validator
docker run -d \
  --name callchain-node \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 51235:51235 \
  -p 9090:9090 \
  ghcr.io/callchain/callchaind:v0.1.0-testnet \
  --config /config/config.toml

# Full Node
docker run -d \
  --name callchain-fullnode \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 51235:51235 \
  -p 9090:9090 \
  ghcr.io/callchain/callchaind:v0.1.0-testnet \
  --config /config/config.toml
```

#### Build Custom Image

```bash
# Clone repo
git clone https://github.com/callchain/call-node.git
cd call-node

# Build with custom features (e.g., light-client-bridge)
cargo build --release --features light-client-bridge

# Build Docker image
docker build -t callchaind:custom .

# Run custom image
docker run -d \
  --name callchain-custom \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 51235:51235 \
  -p 9090:9090 \
  callchaind:custom \
  --config /config/config.toml
```

## Genesis File

Every node needs a genesis file to initialize the chain state on first boot.

```bash
# For devnet / local testing
cp example/genesis.example.json /etc/callchain/genesis.json

# For testnet / mainnet — download from the official release
# curl -o /etc/callchain/genesis.json https://releases.callchain.org/testnet/genesis.json
```

The genesis file defines:
- Chain ID and name
- Initial validators and their stakes
- Initial asset registry (including CALL token)
- Initial balances
- Consensus parameters

See [`docs/genesis.md`](../genesis.md) for the full JSON format specification.

## Configuration

### Validator Node Configuration

```toml
[mode]
mode = "validator"

[keys]
validator_keystore = "/etc/callchain/validator.key"
# OR HashiCorp Vault
# vault_addr = "https://vault.example.com:8200"
# vault_token = "hvs.XXXXXXXX"
# vault_key_name = "callchain-validator"

[genesis]
path = "/etc/callchain/genesis.json"

[p2p]
listen_addr = "0.0.0.0:51235"
bootstrap_peers = [
    "peer_id_1@bootstrap1.callchain.org:51235",
    "peer_id_2@bootstrap2.callchain.org:51235",
]
max_peers = 50

[rpc]
http_addr = "0.0.0.0:8545"
ws_addr = "0.0.0.0:8546"
max_connections = 1000

# Rate limiting
rate_limit_rps = 100
rate_limit_window_secs = 60

[storage]
data_dir = "/var/lib/callchain"
db_cache_size = 2048

[metrics]
addr = "0.0.0.0:9090"

[logging]
level = "info"
format = "json"

[governance]
require_signatures = true

[light_client]
# beacon_url = "https://eth-mainnet.g.alchemy.com/v2/..."
# checkpoint_file = "/etc/callchain/checkpoint.json"
```

> **Light Client Note:** If you enable the Ethereum light client (`beacon_url` set), you must also provide `genesis_validators_root` (32-byte hex). The checkpoint file is optional but recommended for faster sync. See [`for-light-client.md`](for-light-client.md) for full configuration and troubleshooting.

### Full Node Configuration

Full nodes do not participate in consensus. They sync, validate, and serve RPC queries.

```toml
[mode]
mode = "full"

[genesis]
path = "/etc/callchain/genesis.json"

[p2p]
listen_addr = "0.0.0.0:51235"
bootstrap_peers = [
    "peer_id_1@bootstrap1.callchain.org:51235",
    "peer_id_2@bootstrap2.callchain.org:51235",
]
max_peers = 50

[rpc]
http_addr = "0.0.0.0:8545"
ws_addr = "0.0.0.0:8546"
max_connections = 1000

[storage]
data_dir = "/var/lib/callchain"
db_cache_size = 2048
# archive = true

[metrics]
addr = "0.0.0.0:9090"

[logging]
level = "info"
format = "json"
```

**Key differences from validator:**
- No `[keys]` section
- No `validator_keystore` / `vault_*` configuration
- Lower resource requirements
- Can run in `archive` mode to serve historical queries

## Key Management

### Option 1: Encrypted Keystore (Testnet)

```bash
# Generate key
calld wallet generate-keys

# Store in OS keyring
calld wallet store-keyring --key <SECRET_KEY> --service call-node --user validator1
```

### Option 2: HashiCorp Vault (Production)

```bash
vault secrets enable transit
vault write -f transit/keys/callchain-validator
```

```toml
# config.toml
vault_addr = "https://vault.example.com:8200"
vault_token = "hvs.XXXXXXXX"
vault_key_name = "callchain-validator"
```

## Systemd Service

```ini
[Unit]
Description=Callchain Node
After=network.target

[Service]
Type=simple
User=callchain
Group=callchain
ExecStart=/usr/local/bin/calld --config /etc/callchain/config.toml
Restart=always
RestartSec=10
LimitNOFILE=65536
Environment="RUST_LOG=info,callchain=debug"
Environment="CALL_KEYSTORE_PASS_FILE=/etc/callchain/keystore.pass"

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/callchain
ReadWritePaths=/var/log/callchain

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /bin/false callchain
sudo mkdir -p /var/lib/callchain /var/log/callchain /etc/callchain
sudo chown -R callchain:callchain /var/lib/callchain /var/log/callchain

sudo systemctl daemon-reload
sudo systemctl enable callchaind
sudo systemctl start callchaind
```

## Daily Operations

### Start / Stop / Restart

```bash
# Start
sudo systemctl start callchaind

# Stop (graceful — waits for in-flight block production)
sudo systemctl stop callchaind

# Restart
sudo systemctl restart callchaind

# Check status
sudo systemctl status callchaind
```

### Check Sync Status

A new or restarted node must sync blocks from peers before it can serve RPC queries or participate in consensus.

```bash
# Query sync progress via RPC
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_syncing","id":1}' | jq .
```

**Response when syncing:**
```json
{
  "jsonrpc": "2.0",
  "result": {
    "startingBlock": "0x0",
    "currentBlock": "0x1234",
    "highestBlock": "0x5678"
  },
  "id": 1
}
```

**Response when fully synced:**
```json
{ "jsonrpc": "2.0", "result": false, "id": 1 }
```

### Check Latest Block

```bash
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' | jq .
```

### View Live Logs

```bash
# Follow logs via journald
sudo journalctl -u callchaind -f

# Filter for consensus events
sudo journalctl -u callchaind -f | grep "BFT\|consensus\|block"

# JSON logs with jq
sudo tail -f /var/log/callchain/node.log | jq '. | select(.fields.height)'
```

---

## Data Pruning

Callchain stores all data in MDBX (`/var/lib/callchain/mdbx/`). Without pruning, disk usage grows indefinitely. The node supports three operating modes that control retention.

### Node Modes

| Mode | Data Retained | Use Case | Disk Growth |
|------|--------------|----------|-------------|
| `Full` (default) | Current state + recent 100K blocks + 1M receipts | Standard RPC node | ~50 GB/month |
| `Archive` | All historical state, blocks, receipts | Block explorers, indexers | Unbounded |
| `Light` | Last 1,000 blocks only | Edge / IoT deployments | ~1 GB/month |

### Enabling Archive Mode

Archive mode disables all pruning. Enable it before first boot (cannot be toggled on an existing database without a full resync).

**Via CLI:**
```bash
calld --config /etc/callchain/config.toml --archive
```

**Via config:**
```toml
[storage]
data_dir = "/var/lib/callchain"
# snapshot_retention_blocks = u64::MAX  # implied by archive mode
```

### Configuring Pruning Retention

For full / validator nodes, tune retention in `config.toml`:

```toml
[storage]
# Number of recent blocks with full state snapshots (default: 128)
# Set to u64::MAX for archive mode
snapshot_retention_blocks = 128
```

The node also runs automatic layered pruning every 10,000 blocks:

| Layer | Default Retention | What Gets Removed |
|-------|-------------------|-------------------|
| State snapshots | 128 blocks | Old `InMemoryStateProvider` clones |
| Account history | 50,000 blocks | Historical `AccountHistory` diffs |
| Block bodies | 100,000 blocks | Old transaction lists |
| Receipts / logs | 1,000,000 blocks | Old execution receipts |

> **Warning:** Reducing `snapshot_retention_blocks` below 128 will break `eth_call` and `eth_estimateGas` for historical blocks beyond the retention window.

### Manual Pruning

The node prunes automatically during block production. There is no manual prune CLI command. If disk space is critically low:

1. Stop the node: `sudo systemctl stop callchaind`
2. Back up the database
3. Restart with archive mode disabled (if currently enabled)
4. Monitor `metrics_db_size_bytes` to confirm reduction

### Disk Usage Estimates

| Node Type | 1 Month | 6 Months | 1 Year |
|-----------|---------|----------|--------|
| Full (pruned) | ~50 GB | ~200 GB | ~350 GB |
| Archive | ~200 GB | ~1.2 TB | ~2.5 TB |
| Light | ~1 GB | ~6 GB | ~12 GB |

---

## Monitoring

### Prometheus Metrics

Scrape `http://<node>:9090/metrics`:

| Metric | Alert Condition | Threshold |
|--------|----------------|-----------|
| `consensus_blocks_produced` | Flat for > 60s | Consensus stall |
| `p2p_peers` | < 3 peers for > 5min | Network partition |
| `mempool_tx_count` | > 10,000 | Production bottleneck |
| `block_latency_ms` | p99 > 500ms | Performance degradation |
| `node_uptime_seconds` | Reset unexpectedly | Node restart / crash |

### Alerting (Webhook/Slack)

The node includes a built-in alert dispatcher. Configure in your monitoring stack:

**Prometheus Alertmanager rule example:**

```yaml
groups:
  - name: callchain
    rules:
      - alert: ConsensusStall
        expr: rate(consensus_blocks_produced[5m]) == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Consensus stalled on {{ $labels.instance }}"

      - alert: LowPeerCount
        expr: p2p_peers < 3
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Low peer count on {{ $labels.instance }}"

      - alert: HighBlockLatency
        expr: histogram_quantile(0.99, rate(block_latency_ms_bucket[5m])) > 500
        for: 3m
        labels:
          severity: warning
        annotations:
          summary: "High block latency on {{ $labels.instance }}"
```

**Webhook receiver (custom HTTP endpoint):**

```yaml
receivers:
  - name: 'callchain-webhook'
    webhook_configs:
      - url: 'https://alerts.your-ops.com/webhook/callchain'
        send_resolved: true
```

### Health Check

```bash
curl http://localhost:9090/health
```

Returns `200` with subsystem status or `503` if degraded.

### Log Inspection

```bash
# Journald
sudo journalctl -u callchaind -f

# JSON logs
sudo tail -f /var/log/callchain/node.log | jq .
```

## Backup and Recovery

### What to Back Up

A complete backup includes three components:

| Component | Path | Size | Frequency |
|-----------|------|------|-----------|
| MDBX database | `/var/lib/callchain/mdbx/` | Majority of disk usage | Daily |
| Configuration | `/etc/callchain/config.toml` | ~1 KB | On change |
| Validator keys | `/etc/callchain/validator.key` | ~64 B | Once + on rotation |

> **Note:** The `snapshots/` subdirectory inside `data_dir` contains validator-signed state snapshots. Backing these up is optional — they can be regenerated, but keeping them accelerates recovery.

### Regular Backup (Offline)

Stop the node before backing up MDBX to ensure a consistent snapshot:

```bash
sudo systemctl stop callchaind
sudo tar czf /backup/callchain-$(date +%Y%m%d).tar.gz -C /var/lib/callchain .
sudo systemctl start callchaind
```

For large databases, consider using `rsync` for incremental backups instead of full tar archives.

### Automated Backup Script

Create `/etc/callchain/backup.sh`:

```bash
#!/bin/bash
set -e
DATA_DIR="/var/lib/callchain"
BACKUP_DIR="/backup"
DATE=$(date +%Y%m%d_%H%M%S)

# Stop node for consistent backup
systemctl stop callchaind

# Backup data + config
tar czf "${BACKUP_DIR}/callchain-${DATE}.tar.gz" \
  -C "${DATA_DIR}" . \
  -C /etc/callchain config.toml

# Keep only last 7 backups
ls -1t "${BACKUP_DIR}"/callchain-*.tar.gz | tail -n +8 | xargs -r rm -f

# Restart node
systemctl start callchaind

echo "Backup complete: ${BACKUP_DIR}/callchain-${DATE}.tar.gz"
```

Add to crontab for daily 3 AM backups:
```bash
0 3 * * * /etc/callchain/backup.sh >> /var/log/callchain/backup.log 2>&1
```

### Recovery from Snapshot

```bash
sudo systemctl stop callchaind
sudo rm -rf /var/lib/callchain/mdbx
sudo tar xzf /backup/callchain-20260101.tar.gz -C /var/lib/callchain
sudo systemctl start callchaind
```

After recovery, the node will resume from the last persisted height and catch up via sync.

### Disaster Recovery: Re-sync from Genesis

If the database is corrupted and no backup is available:

```bash
sudo systemctl stop callchaind
sudo rm -rf /var/lib/callchain/mdbx/*
sudo systemctl start callchaind
```

The node will re-initialize from genesis and begin syncing blocks from peers. This is much slower than restoring from backup.

## Upgrades

### Pre-Upgrade Checklist

- [ ] Take database snapshot
- [ ] Back up current binary as `calld.backup`
- [ ] Back up config files
- [ ] Announce maintenance window
- [ ] Have >= 2/3 validators coordinate timing

### Rolling Upgrade

```bash
sudo cp /usr/local/bin/calld /usr/local/bin/calld.backup
sudo cp target/release/calld /usr/local/bin/calld
sudo systemctl restart callchaind
curl -s http://localhost:9090/health | jq .
```

### Rollback

```bash
sudo systemctl stop callchaind
sudo cp /usr/local/bin/calld.backup /usr/local/bin/calld
# If schema changed, restore DB from pre-upgrade snapshot
sudo systemctl start callchaind
```

## Data Sync

### Initial Sync for New Nodes

When a new node starts for the first time, it performs an initial sync:

1. **Genesis initialization** — Loads genesis file, creates initial validator set and balances
2. **P2P peer discovery** — Connects to bootstrap peers via `SYNC_CHANNEL = 3`
3. **Block sync** — Requests missing blocks from peers in batches
4. **State validation** — Re-executes each synced block to verify state roots
5. **Live catch-up** — Once near the chain tip, switches to block announcements

**Typical sync speed:** ~100-500 blocks/second depending on network latency and block complexity.

### Fast Sync via State Snapshots

Instead of replaying every block from genesis, a node can fast-sync from a trusted state snapshot:

1. Obtain a validator-signed state snapshot from a trusted source (another operator, snapshot service)
2. Place the snapshot file in `/var/lib/callchain/snapshots/`
3. Start the node — it will detect the snapshot and validate signatures against the current validator set
4. If 2/3 validator signatures are valid, the node fast-forwards to the snapshot height and resumes sync from there

> **Security:** Only use snapshots from trusted sources. An invalid snapshot will cause the node to reject it and fall back to full block sync.

### Catch-Up Sync for Offline Nodes

If a validator or full node was offline for a period:

```bash
# Start the node — it automatically detects the gap
sudo systemctl start callchaind

# Monitor catch-up progress
sudo journalctl -u callchaind -f | grep "sync\|catch-up"
```

The node will:
1. Read its last persisted height from MDBX
2. Query peers for their current height
3. Request missing block ranges via `SyncRequest`
4. Apply each `SyncResponse` batch, validating state roots
5. Resume normal operation once caught up

### Sync Status Monitoring

```bash
# Check if syncing
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_syncing","id":1}' | jq .

# Compare local height to network height
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' | jq .
```

### Troubleshooting Slow Sync

| Symptom | Cause | Fix |
|---------|-------|-----|
| Sync stalls at same height | Missing block in peer responses | Restart node to reconnect to different peers |
| Very slow sync (<10 bps) | High-latency peers or large blocks | Increase `max_peers` in config to find closer peers |
| "State root mismatch" after sync | Corrupt database or invalid snapshot | Wipe `mdbx/` and re-sync from genesis or a fresh snapshot |
| Sync never starts | No peers connected | Check bootstrap peers, firewall rules, network connectivity |

---

## Validator Operations

If you run a validator node, see [`for-validator.md`](for-validator.md) for validator-specific operations:
- Staking, unstaking, and claiming unbonded stake
- Oracle price submission and monitoring
- Bridge deposit attestation
- Key rotation procedures
- Slashing monitoring and alerts

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "MDBX lock contention" | Multiple processes accessing DB | Ensure only one node process per data dir |
| "No peers connected" | Bootstrap peers unreachable | Check network, verify peer IDs |
| "Consensus timeout" | < 2/3 validators online | Check validator health, network partitions |
| "Rate limit exceeded" | Legitimate traffic blocked | Increase `--rate-limit-rps` or whitelist IPs |
| "TLS handshake failed" | Certificate issue | Check cert/key paths, expiry, format |
| High memory usage | Archive mode or large state | Enable pruning (remove `--archive`) |

## Log Rotation

Create `/etc/logrotate.d/callchain`:

```bash
/var/log/callchain/*.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    create 0644 callchain callchain
    sharedscripts
    postrotate
        systemctl reload callchaind || true
    endscript
}
```

Enable:
```bash
sudo logrotate -d /etc/logrotate.d/callchain  # dry run
sudo systemctl restart logrotate
```

## Firewall Rules

```bash
sudo ufw allow 8545/tcp
sudo ufw allow 8546/tcp
sudo ufw allow 51235/tcp
sudo ufw allow from 10.0.0.0/8 to any port 9090
```

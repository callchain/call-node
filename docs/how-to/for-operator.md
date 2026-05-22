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

### Regular Backup

```bash
sudo systemctl stop callchaind
sudo tar czf /backup/callchain-$(date +%Y%m%d).tar.gz -C /var/lib/callchain .
sudo systemctl start callchaind
```

### Recovery from Snapshot

```bash
sudo systemctl stop callchaind
sudo rm -rf /var/lib/callchain/mdbx
sudo tar xzf /backup/callchain-20260101.tar.gz -C /var/lib/callchain
sudo systemctl start callchaind
```

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

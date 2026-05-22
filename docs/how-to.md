# Callchain How-To Guide

This guide covers two perspectives:

1. **Developer** — How to build, test, and contribute to Callchain
2. **Node Operator** — How to deploy, configure, and operate a Callchain node

---

## For Developers

### Prerequisites

- Rust 1.82 or later
- Cargo (comes with Rust)
- Git
- ~8GB RAM for full test suite
- ~2GB disk for build artifacts

### Clone and Build

```bash
git clone https://github.com/callchain/call-node.git
cd call-node

# Debug build
cargo build

# Release build (optimized)
cargo build --release

# The binary is at target/release/calld
```

### Running Tests

```bash
# All tests (single thread avoids MDBX lock contention)
cargo test --workspace -- --test-threads=1

# Specific crate
cargo test -p call-consensus -- --test-threads=1
cargo test -p call-rpc -- --test-threads=1
cargo test -p call-shielded --features nova-prover -- --test-threads=1

# With output
cargo test --workspace -- --test-threads=1 --nocapture
```

### Code Quality

```bash
# Clippy (treat warnings as errors in CI)
cargo clippy --workspace -- -D warnings

# Formatting
cargo fmt --all

# Check formatting without modifying
cargo fmt --all -- --check

# Audit dependencies
cargo audit
```

### Local Devnet (Single Node)

```bash
# Generate a validator key
calld wallet generate-keys
# Save the secret key output

# Run a solo validator (no BFT consensus needed for local dev)
calld --validator --validator-key <SECRET_KEY_HEX> --solo \
  --http-addr 127.0.0.1:8545 \
  --ws-addr 127.0.0.1:8546 \
  --metrics-addr 127.0.0.1:9090 \
  --log-level debug
```

The node will:
- Produce blocks every ~250ms
- Expose HTTP RPC on `8545`
- Expose WebSocket RPC on `8546`
- Expose Prometheus metrics on `9090`

### Local Devnet (Multi-Node)

For a 4-node BFT network on localhost:

```bash
# Node 1 (bootstrap)
calld --validator --validator-key <KEY1> \
  --p2p-listen-addr 127.0.0.1:51235 \
  --http-addr 127.0.0.1:8545 \
  --data-dir ~/.callchain/node1

# Node 2
calld --validator --validator-key <KEY2> \
  --p2p-listen-addr 127.0.0.1:51236 \
  --p2p-bootstrap-peers <PEER1_ID>@127.0.0.1:51235 \
  --http-addr 127.0.0.1:8546 \
  --data-dir ~/.callchain/node2

# Node 3 & 4 follow same pattern
```

### Wallet Operations

```bash
# Check balance
calld wallet balance --address <ADDR> --asset-id 1 --rpc-url http://127.0.0.1:8545

# Send payment
calld wallet send \
  --from-key <SECRET_KEY> \
  --to <RECIPIENT_ADDR> \
  --amount 1000000 \
  --asset-id 1 \
  --nonce 0 \
  --rpc-url http://127.0.0.1:8545

# Server info
calld wallet server-info --rpc-url http://127.0.0.1:8545

# Mempool stats
calld wallet mempool --rpc-url http://127.0.0.1:8545
```

### RPC Examples

```bash
# Eth block number
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'

# Protocol balance
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'

# Server info
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'
```

### Adding a New Crate

1. Create directory under `crates/<name>/`
2. Add `Cargo.toml` with `workspace = true`
3. Add to root `Cargo.toml` workspace members
4. Add to `workspace.dependencies` if other crates will depend on it
5. Run `cargo check --workspace`

### Running Benchmarks

```bash
# All benchmarks
cargo bench --workspace

# Specific benchmark
cargo bench -p call-precompile --bench precompile_execute
cargo bench -p call-consensus --bench block_production
cargo bench -p call-crypto --bench signature_verify
```

### Fuzz Testing

```bash
# Run a fuzz target
cargo fuzz run tx_rlp_decode
cargo fuzz run precompile_dispatch
cargo fuzz run balance_arithmetic
```

### Contributing

1. Fork and branch from `main`
2. Write tests for new code
3. Run `cargo clippy --workspace -- -D warnings`
4. Run `cargo test --workspace -- --test-threads=1`
5. Open a PR against `main`

---

## For Node Operators

### System Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| CPU | 4 cores | 8+ cores |
| RAM | 16 GB | 32 GB |
| Disk (SSD) | 500 GB | 1 TB NVMe |
| Network | 100 Mbps | 1 Gbps |
| OS | Linux | Ubuntu 22.04 LTS |

### Installation

#### From Source

```bash
git clone https://github.com/callchain/call-node.git
cd call-node
cargo build --release

# Install to system
sudo cp target/release/calld /usr/local/bin/
sudo chmod +x /usr/local/bin/calld
```

#### Docker

```bash
docker pull ghcr.io/callchain/callchaind:v0.1.0-testnet

docker run -d \
  --name callchain-node \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 30303:30303 \
  -p 9090:9090 \
  ghcr.io/callchain/callchaind:v0.1.0-testnet \
  --config /config/config.toml
```

### Configuration

Create `/etc/callchain/config.toml`:

```toml
[mode]
# "solo" = single-node producer (dev/test)
# "validator" = BFT consensus validator
# "full" = non-validating full node
mode = "validator"

[keys]
# Production: use keystore or Vault
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

# Enable TLS for HTTPS/WSS
# tls_cert_path = "/etc/callchain/cert.pem"
# tls_key_path = "/etc/callchain/key.pem"

# Rate limiting
rate_limit_rps = 100
rate_limit_window_secs = 60

[storage]
data_dir = "/var/lib/callchain"
db_cache_size = 2048
# archive = true  # Keep all history (disables pruning)

[metrics]
addr = "0.0.0.0:9090"

[logging]
level = "info"
format = "json"
# retention_days = 30

[governance]
require_signatures = true

[light_client]
# beacon_url = "https://eth-mainnet.g.alchemy.com/v2/..."
# checkpoint_file = "/etc/callchain/checkpoint.json"
```

### Key Management

#### Option 1: Encrypted Keystore (Recommended for Testnet)

```bash
# Generate key
calld wallet generate-keys

# Store in OS keyring
calld wallet store-keyring --key <SECRET_KEY> --service call-node --user validator1

# Use keyring in config (no plaintext keys)
```

#### Option 2: HashiCorp Vault (Recommended for Production)

```bash
# Enable transit engine
vault secrets enable transit

# Create signing key
vault write -f transit/keys/callchain-validator

# Node config
# vault_addr = "https://vault.example.com:8200"
# vault_token = "hvs.XXXXXXXX"
# vault_key_name = "callchain-validator"
```

### Systemd Service

Create `/etc/systemd/system/callchaind.service`:

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
Environment="RUST_LOG=info,callchain=debug"
Environment="CALL_KEYSTORE_PASS_FILE=/etc/callchain/keystore.pass"

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/callchain
ReadWritePaths=/var/log/callchain

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo useradd -r -s /bin/false callchain
sudo mkdir -p /var/lib/callchain /var/log/callchain /etc/callchain
sudo chown -R callchain:callchain /var/lib/callchain /var/log/callchain

sudo systemctl daemon-reload
sudo systemctl enable callchaind
sudo systemctl start callchaind
sudo systemctl status callchaind
```

### Monitoring

#### Prometheus Metrics

Scrape `http://<node>:9090/metrics`:

| Metric | Alert Condition |
|--------|----------------|
| `consensus_blocks_produced` | Flat for > 60s = consensus stall |
| `p2p_peers` | < min_healthy_peers = network issue |
| `mempool_tx_count` | > 10,000 = production bottleneck |
| `node_uptime_seconds` | — |
| `block_latency_ms` | p99 > 500ms = performance issue |

#### Health Check

```bash
curl http://localhost:9090/health
```

Returns `200` with subsystem status or `503` if degraded.

#### Log Inspection

```bash
# Journald (if using systemd)
sudo journalctl -u callchaind -f

# JSON logs (if configured)
sudo tail -f /var/log/callchain/node.log | jq .
```

### Backup and Recovery

#### Regular Backup

```bash
# Stop node
sudo systemctl stop callchaind

# Backup data directory
sudo tar czf /backup/callchain-$(date +%Y%m%d).tar.gz -C /var/lib/callchain .

# Restart node
sudo systemctl start callchaind
```

#### Recovery from Snapshot

```bash
# Stop node
sudo systemctl stop callchaind

# Remove corrupted data
sudo rm -rf /var/lib/callchain/mdbx

# Restore from backup
sudo tar xzf /backup/callchain-20260101.tar.gz -C /var/lib/callchain

# Restart
sudo systemctl start callchaind
```

### Upgrades

#### Pre-Upgrade Checklist

- [ ] Take database snapshot
- [ ] Back up current binary as `calld.backup`
- [ ] Back up config files
- [ ] Announce maintenance window
- [ ] Have >= 2/3 validators coordinate timing

#### Rolling Upgrade

```bash
# 1. Back up
sudo cp /usr/local/bin/calld /usr/local/bin/calld.backup

# 2. Deploy new binary
sudo cp target/release/calld /usr/local/bin/calld

# 3. Restart
sudo systemctl restart callchaind

# 4. Verify
sudo systemctl status callchaind
curl -s http://localhost:9090/health | jq .
```

#### Rollback

```bash
sudo systemctl stop callchaind
sudo cp /usr/local/bin/calld.backup /usr/local/bin/calld
# If schema changed, restore DB from pre-upgrade snapshot
sudo systemctl start callchaind
```

### Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "MDBX lock contention" | Multiple processes accessing DB | Ensure only one node process per data dir |
| "No peers connected" | Bootstrap peers unreachable | Check network, verify peer IDs |
| "Consensus timeout" | < 2/3 validators online | Check validator health, network partitions |
| "Rate limit exceeded" | Legitimate traffic blocked | Increase `--rate-limit-rps` or whitelist IPs |
| "TLS handshake failed" | Certificate issue | Check cert/key paths, expiry, format |
| High memory usage | Archive mode or large state | Enable pruning (remove `--archive`) |

### Firewall Rules

```bash
# RPC (HTTP + WebSocket)
sudo ufw allow 8545/tcp
sudo ufw allow 8546/tcp

# P2P
sudo ufw allow 51235/tcp

# Metrics (restrict to monitoring server)
sudo ufw allow from 10.0.0.0/8 to any port 9090
```

---

## Reference

- [Release Guide](release.md) — Detailed release process, security audit scope, performance baselines
- [Observability](observability.md) — Metrics, tracing, logging, alerting deep dive
- [Runbooks](runbooks/) — Incident response procedures
- [Specification](spec.md) — Full protocol specification

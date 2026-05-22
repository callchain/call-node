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

```bash
docker pull ghcr.io/callchain/callchaind:v0.1.0-testnet

# Validator
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

| Metric | Alert Condition |
|--------|----------------|
| `consensus_blocks_produced` | Flat for > 60s = consensus stall |
| `p2p_peers` | < min_healthy_peers = network issue |
| `mempool_tx_count` | > 10,000 = production bottleneck |
| `block_latency_ms` | p99 > 500ms = performance issue |

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

### Oracle Price Submission

As a validator, your node automatically participates in the decentralized price oracle. No manual intervention is required during normal operation.

**How it works:**
1. Block producer broadcasts `OraclePriceRequest` on P2P channel 4 at each oracle update interval (every 1,000 blocks)
2. Your validator fetches prices from configured sources (HTTP / local)
3. Signs an `OraclePriceSubmission` with Ed25519: `(validator_id, pair, price, block_number, timestamp)`
4. Submits via P2P to the block producer
5. Block producer aggregates submissions, computes median, detects outliers, and writes result to EVM storage via precompile `0x101`

**Operator responsibilities:**
- Ensure your node has outbound internet access to price data sources (Binance, Coinbase, etc.)
- Monitor for outlier flags: prices deviating > 5% from median will mark your validator as an outlier
- Repeated outlier submissions may lead to slashing (not yet implemented, but tracked in-memory)

**Check current tracked pairs:**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_oracleTrackedPairs","id":1}'
```

**Check latest price:**
```bash
# CALL/USD (pair base=1, quote=0)
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_oracleGetPrice","params":[1,0],"id":1}'
```

Prices use **6 decimal places** (e.g. `$2.00` = `2_000_000`).

### Bridge Validator Operations

As a validator, you participate in the cross-chain bridge by attesting to Ethereum deposit events.

**Deposit attestation flow:**
1. Monitor Ethereum bridge contract for `BridgeDeposit` events (via Ethereum RPC)
2. When an event is observed, compute the event hash:
   ```
   event_hash = keccak256(
       chain_id ||
       source_tx_hash ||
       source_block_number ||
       sender ||
       recipient ||
       asset_id ||
       amount
   )
   ```
3. Sign the event hash with your validator **secp256k1** key
4. Submit the signed attestation to a designated aggregator validator
5. Once 14+ distinct validator signatures are collected (2/3 of 21), the aggregator calls `externalDeposit()` on precompile `0x103`

**Operator responsibilities:**
- Ensure your node has access to an Ethereum RPC endpoint (mainnet or Arbitrum)
- Monitor bridge metrics: pending deposits, daily limit usage, challenge status
- Be prepared to respond to challenges during the challenge period (~14 days)

**Check bridge status:**
```bash
# Total deposits
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0x0000000000000000000000000000000000000103","data":"0x..."},"latest"],"id":1}'

# Check if bridge is paused
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_bridgeIsPaused","id":1}'
```

**Submit external deposit (aggregator only, 14+ signatures required):**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...externalDeposit ABI with signatures..."
    }],
    "id": 1
  }'
```

**Key requirements:**
- Caller must be a registered validator
- `sourceTxHash` must not have been processed before
- Asset must be in the `allowed_assets` whitelist
- Amount must not exceed `max_per_tx` or `daily_limit_per_asset`
- Minimum 14 signatures from distinct validators

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "MDBX lock contention" | Multiple processes accessing DB | Ensure only one node process per data dir |
| "No peers connected" | Bootstrap peers unreachable | Check network, verify peer IDs |
| "Consensus timeout" | < 2/3 validators online | Check validator health, network partitions |
| "Rate limit exceeded" | Legitimate traffic blocked | Increase `--rate-limit-rps` or whitelist IPs |
| "TLS handshake failed" | Certificate issue | Check cert/key paths, expiry, format |
| High memory usage | Archive mode or large state | Enable pruning (remove `--archive`) |

## Firewall Rules

```bash
sudo ufw allow 8545/tcp
sudo ufw allow 8546/tcp
sudo ufw allow 51235/tcp
sudo ufw allow from 10.0.0.0/8 to any port 9090
```

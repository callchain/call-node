# How-To: Run a Local Devnet with Docker Compose

This guide covers running a local Callchain devnet using Docker Compose. Two setups are provided:

- **Single-node devnet** (`docker-compose.yml` at repo root) — quick local development and testing.
- **Multi-node devnet** (`devnet/docker-compose.yml`) — 6-node BFT network for consensus testing and multi-validator scenarios.

---

## Prerequisites

- [Docker](https://docs.docker.com/engine/install/) 24.0+
- [Docker Compose](https://docs.docker.com/compose/install/) v2.20+ (or the `docker compose` plugin)
- ~4 GB RAM available to Docker
- ~2 GB free disk space

Verify your installation:

```bash
docker --version
docker compose version
```

---

## Quick Start (Single-Node Devnet)

The root `docker-compose.yml` builds the node from source and starts a single validator with example genesis and config files.

```bash
git clone https://github.com/callchain/call-node.git
cd call-node

# Build image and start the node
docker compose up --build -d

# View logs
docker compose logs -f node1

# Check health
docker compose ps
```

The node exposes the following ports on `localhost`:

| Service | Port | Description |
|---------|------|-------------|
| HTTP RPC | `5005` | JSON-RPC over HTTP |
| WebSocket RPC | `5006` | JSON-RPC over WebSocket |
| P2P | `51235` | Peer-to-peer networking |
| Metrics | `9090` | Prometheus metrics endpoint |

Test the RPC endpoint:

```bash
curl -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'
```

Stop the node:

```bash
docker compose down
```

Stop and remove the data volume:

```bash
docker compose down -v
```

---

## Multi-Node Devnet Setup

The `devnet/` directory contains a pre-configured 6-node BFT network. Each node runs as a validator with fixed IP addresses on a dedicated Docker bridge network.

### Build the Image

The multi-node setup uses a pre-built image tag (`callchain/calld:latest`). Build it first from the repo root:

```bash
cd call-node
docker build -t callchain/calld:latest .
```

### Start the Network

```bash
cd devnet
docker compose up -d
```

All six nodes start in parallel and connect to each other via the static bootstrap peer lists defined in their config files.

### Verify the Network

```bash
# List running containers
docker compose ps

# View logs for a specific node
docker compose logs -f node1

# Check peer connections across the network
for port in 5005 5007 5009 5011 5013 5015; do
  echo "=== node on port $port ==="
  curl -s -X POST "http://127.0.0.1:$port" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"server_info","id":1}' | jq .
done
```

### Port Mapping

| Node | HTTP RPC | WebSocket RPC | P2P (host) | Metrics |
|------|----------|---------------|------------|---------|
| node1 | `5005` | `5006` | `51235` | `9090` |
| node2 | `5007` | `5008` | `51236` | `9091` |
| node3 | `5009` | `5010` | `51237` | `9092` |
| node4 | `5011` | `5012` | `51238` | `9093` |
| node5 | `5013` | `5014` | `51239` | `9094` |
| node6 | `5015` | `5016` | `51240` | `9095` |

Internal Docker network: `172.28.0.0/16`

### Stop the Network

```bash
docker compose down
```

Stop and wipe all node data:

```bash
docker compose down -v
```

---

## Available Services and Ports

### Default Ports (Container Internal)

| Port | Protocol | Purpose |
|------|----------|---------|
| `5005` | TCP | HTTP JSON-RPC |
| `5006` | TCP | WebSocket JSON-RPC |
| `51235` | TCP | P2P consensus and sync |
| `9090` | TCP | Prometheus metrics (`/metrics`) and health (`/health`) |

### Health Checks

The single-node compose file includes a Docker health check that polls `server_info` every 30 seconds. View health status with:

```bash
docker compose ps
# or
docker inspect --format='{{.State.Health.Status}}' <container_name>
```

---

## Interacting with the Devnet

### RPC (HTTP)

```bash
# Server info
curl -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'

# Latest block number
curl -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'

# Protocol balance
curl -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'
```

### WebSocket

```bash
# Using websocat (or wscat)
websocat ws://localhost:5006
```

Subscribe to events by sending JSON-RPC messages over the WebSocket connection.

### Metrics

Scrape Prometheus metrics from any node:

```bash
curl http://localhost:9090/metrics
```

Key metrics to watch:

| Metric | Description |
|--------|-------------|
| `consensus_blocks_produced` | Blocks produced since startup |
| `p2p_peers` | Number of connected peers |
| `mempool_tx_count` | Transactions in mempool |
| `block_latency_ms` | Block production latency |

Health endpoint:

```bash
curl http://localhost:9090/health
```

### CLI Wallet (Inside Container)

```bash
# Server info via CLI
docker compose exec node1 calld wallet server-info --rpc-url http://127.0.0.1:5005

# Mempool stats
docker compose exec node1 calld wallet mempool --rpc-url http://127.0.0.1:5005
```

---

## Resetting / Rebuilding the Devnet

### Reset Data Only

This keeps the image but wipes chain state, forcing re-initialization from genesis on next start.

```bash
# Single-node
docker compose down -v
docker compose up -d

# Multi-node
cd devnet
docker compose down -v
docker compose up -d
```

### Rebuild After Code Changes

```bash
# Single-node (builds from Dockerfile)
docker compose up --build -d

# Multi-node
docker build -t callchain/calld:latest ..
cd devnet
docker compose up -d
```

### Full Clean Slate

```bash
# Remove containers, volumes, and unused images
docker compose down -v --rmi local
```

---

## Customizing the Devnet

### Genesis File

Both compose files mount a genesis JSON file read-only into `/etc/callchain/genesis.json`.

- Single-node: `example/genesis.example.json`
- Multi-node: `devnet/genesis.json`

Edit the genesis file to change initial balances, validators, or chain parameters. See [`docs/genesis.md`](../genesis.md) for the full format.

### Node Configuration

Config files are mounted read-only into `/etc/callchain/config.toml`.

- Single-node: `example/config.example.toml`
- Multi-node: `devnet/configs/node{1..6}.toml`

Common customizations:

```toml
[rpc]
# Change listen address inside container (must match compose port mapping)
http_addr = "0.0.0.0:5005"
ws_addr = "0.0.0.0:5006"
max_connections = 200

[p2p]
max_peers = 100
# For local docker networks, allow_private_ips must be true
allow_private_ips = true

[logging]
level = "debug"   # trace, debug, info, warn, error
format = "json"   # text or json

[metrics]
addr = "0.0.0.0:9090"
```

### Adding Nodes to the Single-Node Setup

Uncomment the `node2` block in the root `docker-compose.yml`:

```yaml
  node2:
    build: .
    ports:
      - "5007:5005"
      - "5008:5006"
      - "51236:51235"
    volumes:
      - node2-data:/var/lib/callchain
      - ./example/config.example.toml:/etc/callchain/config.toml:ro
      - ./example/genesis.example.json:/etc/callchain/genesis.json:ro
    command: >
      --data-dir /var/lib/callchain
      --config /etc/callchain/config.toml
      --genesis-path /etc/callchain/genesis.json
      --p2p-listen-addr 0.0.0.0:51235
      --p2p-bootstrap-peers node1@node1:51235
      --log-level info
    depends_on:
      - node1
    restart: unless-stopped
```

Add the volume:

```yaml
volumes:
  node1-data:
  node2-data:
```

Then restart:

```bash
docker compose up -d
```

### Environment Variables

The `calld` binary does not read environment variables directly for configuration. Pass settings via the `command` override in `docker-compose.yml` or mount a custom `config.toml`.

---

## Common Issues

### Port Already in Use

```
Error response from daemon: Ports are not available: exposing port TCP 0.0.0.0:5005 -> 0.0.0.0:0: listen tcp 0.0.0.0:5005: bind: address already in use
```

**Fix:** Find and stop the process using the port, or change the host port mapping in `docker-compose.yml` (e.g., `5005:5005` -> `15005:5005`).

```bash
lsof -i :5005
kill <PID>
```

### Image Build Fails (OOM)

The Dockerfile installs `libclang-dev` and `clang` in a separate layer to avoid exhausting Docker builder memory. If builds still fail:

```bash
# Increase Docker Desktop memory limit (Settings > Resources)
# Or build with reduced parallelism
docker build --build-arg CARGO_BUILD_JOBS=2 -t callchain/calld:latest .
```

### Nodes Cannot Connect to Each Other

In the multi-node setup, nodes connect via static IPs on the `devnet` bridge network. If peers show `0`:

1. Verify all containers are on the same network:
   ```bash
   docker network inspect callchain-devnet_devnet
   ```
2. Check that `allow_private_ips = true` is set in each node's config (required for RFC1918 Docker subnets).
3. Verify bootstrap peer strings match the expected `peer_id@ip:port` format in each config.

### Container Unhealthy

If the health check fails repeatedly:

```bash
# Check logs for startup errors
docker compose logs node1

# Verify RPC is reachable from inside the container
docker compose exec node1 calld wallet server-info --rpc-url http://127.0.0.1:5005
```

Common causes: genesis file missing, config syntax error, or port binding failure inside the container.

### Slow First Startup

The initial `docker compose up --build` compiles the Rust binary from source. This can take 10-30 minutes depending on hardware. Subsequent starts use the cached image and are near-instant.

### Data Not Persisting

Ensure you do not run `docker compose down -v` unless you intend to wipe state. Normal `docker compose down` preserves named volumes. Verify volumes exist:

```bash
docker volume ls | grep callchain
```

### Permission Denied on Volume Mounts

The runtime stage runs as a non-root `calld` user (UID/GID created in the Dockerfile). If host-mounted volumes cause permission issues, ensure the container user can read the mounted files, or adjust file permissions on the host.

---

## See Also

- [`for-developer.md`](for-developer.md) — Building from source, local binary devnet, and wallet operations
- [`for-operator.md`](for-operator.md) — Production deployment, monitoring, and backup procedures
- [`docs/genesis.md`](../genesis.md) — Genesis file format and configuration

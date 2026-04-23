# Callchain Devnet

6-node devnet running via Docker Compose: 4 validators with BFT consensus + 2 full nodes syncing finalized blocks.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                   Docker Network: devnet                            │
│                                                                     │
│  Validators (BFT consensus via commonware-consensus)               │
│  ─────────────────────────────────────────────────                 │
│  node1  172.28.0.11  Validator  RPC :5005/:5006  P2P :51235  Metrics :9090  │
│  node2  172.28.0.12  Validator  RPC :5007/:5008  P2P :51236  Metrics :9091  │
│  node3  172.28.0.13  Validator  RPC :5009/:5010  P2P :51237  Metrics :9092  │
│  node4  172.28.0.14  Validator  RPC :5011/:5012  P2P :51238  Metrics :9093  │
│                                                                     │
│  Full Nodes (sync finalized blocks)                                │
│  ─────────────────────────────────────────────────                 │
│  node5  172.28.0.15  Full        RPC :5013/:5014  P2P :51239  Metrics :9094  │
│  node6  172.28.0.16  Full        RPC :5015/:5016  P2P :51240  Metrics :9095  │
└─────────────────────────────────────────────────────────────────────┘
```

## Quick Start

```bash
# Build the Docker image first (required after source changes)
./scripts/build-image.sh

# Start all 6 nodes
./devnet/scripts/start.sh

# Check status
./devnet/scripts/status.sh

# Follow logs
./devnet/scripts/logs.sh node1 -f

# Query a node
./devnet/scripts/query.sh 1

# Stop
./devnet/scripts/stop.sh

# Full reset (delete all data)
./devnet/scripts/clean.sh
```

## Port Mapping

| Node | Mode | HTTP RPC | WS RPC | P2P | Metrics |
|------|------|----------|--------|-----|---------|
| 1 | Validator | 5005 | 5006 | 51235 | 9090 |
| 2 | Validator | 5007 | 5008 | 51236 | 9091 |
| 3 | Validator | 5009 | 5010 | 51237 | 9092 |
| 4 | Validator | 5011 | 5012 | 51238 | 9093 |
| 5 | Full | 5013 | 5014 | 51239 | 9094 |
| 6 | Full | 5015 | 5016 | 51240 | 9095 |

## Genesis

The devnet genesis (`genesis.json`) initializes:

- **4 validators**, each with 1M CALL self-stake
- **4 funded accounts**, each with 1M CALL initial balance
- Chain starts at timestamp 1700000000000

### Validator Key Pairs (devnet only — deterministic)

| Node | Address | Secp256k1 Privkey | Ed25519 Seed | Ed25519 Pubkey | Stake |
|------|---------|-------------------|--------------|----------------|-------|
| 1 | `0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266` | `ac09...ff80` | `0x00...01` | `4cb5...ba29` | 1M CALL |
| 2 | `0x70997970C51812dc3A010C7d01b50e0d17dc79C8` | `59c6...690d` | `0x00...02` | `7422...2674` | 1M CALL |
| 3 | `0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC` | `5de4...365a` | `0x00...03` | `f381...a54b` | 1M CALL |
| 4 | `0x90F79bf6EB2c4f870365E785982E1f101E93b906` | `7c85...07a6` | `0x00...04` | `fd50...329b` | 1M CALL |

## Single-Node Devnet

For isolated testing without consensus overhead, use the single-node devnet:

```bash
./devnet/single/scripts/start.sh
```

See `devnet/single/README.md` for details.

## Troubleshooting

```bash
# Check if Docker is running
docker ps

# Check if all containers are healthy
docker compose -f devnet/docker-compose.yml ps

# View full logs
./devnet/scripts/logs.sh

# Restart a single node
docker compose -f devnet/docker-compose.yml restart node2

# Rebuild Docker image after source changes
./scripts/build-image.sh
```

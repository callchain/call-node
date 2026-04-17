# Callchain Devnet

4-node devnet running via Docker Compose.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                   Docker Network: devnet                │
│                                                         │
│  node1  172.28.0.11  (bootstrap)                        │
│    ├── RPC  :5005  WS :5006  P2P :51235  Metrics :9090  │
│                                                         │
│  node2  172.28.0.12                                     │
│    ├── RPC  :5007  WS :5008  P2P :51236  Metrics :9091  │
│                                                         │
│  node3  172.28.0.13                                     │
│    ├── RPC  :5009  WS :5010  P2P :51237  Metrics :9092  │
│                                                         │
│  node4  172.28.0.14                                     │
│    ├── RPC  :5011  WS :5012  P2P :51238  Metrics :9093  │
└─────────────────────────────────────────────────────────┘
```

All 4 nodes are validators with equal stake (100k CALL). Node1 is the bootstrap peer; nodes 2-4 connect to it.

## Quick Start

```bash
# Start all 4 nodes (builds image first)
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

| Node | HTTP RPC | WS RPC | P2P    | Metrics |
|------|----------|--------|--------|---------|
| 1    | 5005     | 5006   | 51235  | 9090    |
| 2    | 5007     | 5008   | 51236  | 9091    |
| 3    | 5009     | 5010   | 51237  | 9092    |
| 4    | 5011     | 5012   | 51238  | 9093    |

## Genesis

The devnet genesis (`genesis.json`) initializes:

- **4 validators**, each with 100k CALL self-stake
- **4 funded accounts**, each with 1M CALL initial balance
- Chain starts at timestamp 1000000

### Validator Key Pairs (devnet only — deterministic)

| Node | Address | Pubkey (hex) | Stake |
|------|---------|--------------|-------|
| 1 | `0x00...01` | `0x0101...01` (32 bytes) | 100k CALL |
| 2 | `0x00...02` | `0x0202...02` (32 bytes) | 100k CALL |
| 3 | `0x00...03` | `0x0303...03` (32 bytes) | 100k CALL |
| 4 | `0x00...04` | `0x0404...04` (32 bytes) | 100k CALL |

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

# Rebuild from source
./devnet/scripts/build.sh
```

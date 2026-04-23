# Callchain Single-Node Devnet

Isolated single-node network for local testing and development. No consensus overhead — the node produces blocks locally via `block_production_loop()`.

## Quick Start

```bash
# Start the node (builds image first)
./devnet/single/scripts/start.sh

# Stop
./devnet/single/scripts/stop.sh
```

## Ports

| Service | Port |
|---------|------|
| HTTP RPC | 5005 |
| WebSocket RPC | 5006 |
| P2P | 51235 |
| Metrics | 9090 |

## Query

```bash
# Server info
curl http://127.0.0.1:5005 -X POST -d '{"jsonrpc":"2.0","method":"server_info","id":1}'

# Latest block
curl http://127.0.0.1:5005 -X POST -d '{"jsonrpc":"2.0","method":"ledger_current","id":1}'
```

## Notes

- No bootstrap peers — this is a standalone network.
- No validator keys required — runs in `mode = "full"` with local block production.
- Reuses the same `genesis.json` as the multi-node devnet.
- Data is persisted in a Docker volume (`node1-data`).

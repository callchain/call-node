# CLI Reference

The `calld` binary is the Callchain node application. This document covers
command-line usage, key arguments, and wallet subcommands.

## Basic Usage

```bash
# Start node with config file
calld --config /etc/callchain/config.toml

# Start validator with keystore (production)
calld --validator --validator-keystore /etc/callchain/validator.key --validator-keystore-pass-file /etc/callchain/keystore.pass

# Start with HashiCorp Vault signing
calld --validator --vault-addr https://vault.example.com:8200 --vault-token $VAULT_TOKEN --vault-key-name callchain-validator

# Start with custom RPC and P2P addresses
calld --http-addr 0.0.0.0:8545 --ws-addr 0.0.0.0:8546 --p2p-listen-addr 0.0.0.0:51235

# Start with TLS
calld --tls-cert-path /etc/callchain/cert.pem --tls-key-path /etc/callchain/key.pem

# Enable rate limiting
calld --rate-limit-rps 100 --rate-limit-window-secs 60
```

## Wallet Commands

```bash
calld wallet generate-keys
calld wallet balance --address <ADDR> --rpc-url http://127.0.0.1:8545
calld wallet send --from-key <KEY> --to <ADDR> --amount 100 --asset-id 1 --nonce 0 --rpc-url http://127.0.0.1:8545
calld wallet server-info --rpc-url http://127.0.0.1:8545
calld wallet mempool --rpc-url http://127.0.0.1:8545
```

## Key Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--validator` | false | Run as validator (requires key) |
| `--solo` | false | Single-node validator without BFT |
| `--validator-key` | — | Hex-encoded consensus key (devnet only) |
| `--validator-keystore` | — | Path to encrypted keystore |
| `--vault-addr` | — | HashiCorp Vault URL |
| `--vault-key-name` | — | Vault transit key name |
| `--p2p-listen-addr` | `0.0.0.0:51235` | P2P listen address |
| `--p2p-bootstrap-peers` | — | Comma-separated peers |
| `--http-addr` | `127.0.0.1:8545` | HTTP RPC listen address |
| `--ws-addr` | `127.0.0.1:8546` | WebSocket RPC listen address |
| `--metrics-addr` | `0.0.0.0:9090` | Prometheus metrics endpoint |
| `--data-dir` | `~/.callchain` | Chain data directory |
| `--db-cache-size` | 1024 | DB cache size in MB |
| `--archive` | false | Keep all history (disable pruning) |
| `--log-level` | `info` | Log level (trace/debug/info/warn/error) |
| `--log-format` | `text` | Log format (text/json) |
| `--rate-limit-rps` | — | Per-IP max requests per window |
| `--rate-limit-window-secs` | 60 | Rate limit window |
| `--tls-cert-path` | — | TLS certificate (PEM) |
| `--tls-key-path` | — | TLS private key (PEM) |
| `--config` | — | TOML config file path |

## RPC Quick Reference

The node exposes standard JSON-RPC over HTTP (default port `8545`) and
WebSocket (default port `8546`):

```bash
# Server info
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'

# Account balance
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'

# Eth block number
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'
```

See [`rpc.md`](rpc.md) for the full RPC server documentation and
[`eth_rpc.md`](eth_rpc.md) for Ethereum-compatible methods.

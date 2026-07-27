# Architecture

Callchain features a unified EVM execution layer with a single consensus
validator set. All transactions execute within the EVM, with protocol-level
operations accessed via native precompiles at fixed addresses (`0x101`–`0x209`).

```
                    Callchain L1
              Simplex BFT Consensus (Single Validator Set)
                         │
                         ▼
                    EVM Contract
                 (Unified Execution Layer)
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
   Protocol State    ERC-20 Storage   Precompiles
   (Native Balance   (Contract        (0x101-0x209)
    Mapping)          Independent
                      Balances)
          │              │
          └──────────┬───┘
                     ▼
            Internal Bridge
           (Switch Precompile 0x207)
           Escrow or Mint/Burn per Asset
```

## Workspace Crates

| Crate | Description |
|-------|-------------|
| `call-primitives` | Core types: Address, Hash, TxHash, BlockHash |
| `call-crypto` | Cryptographic primitives: keccak256, secp256k1, ed25519, BLS |
| `call-serialization` | Binary encoding and decoding (postcard) |
| `call-storage` | reth-db (MDBX) persistence layer |
| `call-protocol` | Balance engine, asset registry, compliance, fees |
| `call-evm` | EVM execution via revm |
| `call-consensus` | Simplex BFT consensus, block production, slashing |
| `call-network` | P2P networking via commonware-p2p |
| `call-mempool` | Mempool, transaction validation, eviction |
| `call-rpc` | JSON-RPC server (HTTP + WebSocket), rate limiting, TLS |
| `call-bridge` | Cross-chain bridge state, deposits, challenges |
| `call-shielded` | Halo2 ZK circuits, shielded transaction proofs |
| `call-agent` | AI agent registration, delegation, batch payments |
| `call-precompile` | EVM precompiled contracts (`0x101`–`0x209`) |
| `call-governance` | Proposal lifecycle, voting, timelock, emergency pause |
| `call-light-client` | Ethereum beacon chain light client verification |
| `call-oracle` | Price feed oracle with P2P aggregation |
| `call-chainspec` | Genesis configuration, chain parameters |
| `call-validator` | Validator staking, set management, key rotation |
| `call-compliance` | Sanction list, compliance data sync |
| `call-asset` | Asset registration, metadata |
| `call-switch` | Internal bridge (escrow / mint-burn) |
| `call-node` | Node application, CLI, boot sequence, telemetry |

## Further Reading

- [`spec.md`](spec.md) — Complete protocol specification
- [`consensus.md`](consensus.md) — Simplex BFT consensus details
- [`evm.md`](evm.md) — EVM execution layer
- [`storage.md`](storage.md) — MDBX persistence layer
- [`precompile.md`](precompile.md) — Precompile reference
- [`adr/`](adr/) — Architecture decision records

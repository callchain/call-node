# Callchain Production Readiness Summary

> Generated: 2026-05-07
> Based on: docs/unready.md, docs/testing.md, docs/security.md, docs/future.md

---

## Bottom Line

**Devnet / internal testnet:** Ready. Code runs, ~1,682 tests pass, blocks produce.

**Public testnet:** Not ready. Missing security hardening, performance validation, TLS/auth, and CI/CD.

**Mainnet:** Not ready. Missing formal audit, DB migration framework, light client BLS verification, Byzantine consensus tests, and long-running testnet validation.

---

## Critical (Must Resolve Before Mainnet)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 1 | **No formal security audit** | Entire codebase | No third-party review of consensus, cryptography, or economic incentives |
| 2 | ~~**No TLS/HTTPS tests**~~ ✅ | `crates/rpc` | Integration tests added: handshake success, plain HTTP rejection, expired cert rejection (`crates/rpc/tests/tls_integration.rs`) |
| 3 | **No auth/authz tests** | `crates/rpc` | No API key, JWT, or IP allowlist coverage |
| 4 | **No MDBX integration tests** | `crates/storage` | Table descriptors exist; no actual read/write/delete tests |
| 5 | **No concurrent DB / corruption recovery tests** | `crates/storage`, `crates/node` | No crash recovery, WAL, or checkpoint tests |
| 6 | **No network partition tests** | `crates/node/tests` | E2E uses local harness only; no Byzantine/equivocation tests |
| 7 | **No light client consensus verification** | `crates/light-client` | No Ethereum BLS signature verification; no malicious fork tests |
| 8 | **No oracle signature negative tests** | `crates/oracle` | Submissions accept any 64-byte signature without validation |
| 9 | **No database migration framework** | `crates/storage` | Schema changes require manual migration or full resync |
| 10 | **No formal verification for shielded circuits** | `crates/shielded` | Groth16 tested but not formally verified |

---

## High (Must Resolve Before Public Testnet)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 11 | **No `eth_getLogs` performance tests** | `crates/rpc` | Linear scan O(n); untested at 1M+ blocks |
| 12 | **No receipt persistence tests** | `crates/node` | Receipts in-memory only; no DB persistence coverage |
| 13 | **No agent block-production integration tests** | `crates/consensus` | Agent txs exist as library but not verified in real blocks |
| 14 | **No slashing economic penalty tests** | `crates/consensus` | Double-sign detected but stake reduction not verified |
| 15 | **No upgrade persistence tests** | `crates/consensus` | `ForkManager` persisted but restart-activation untested |
| 16 | **No multi-upgrade-at-same-height tests** | `crates/consensus` | `check_upgrades_at_height` behavior with multiple upgrades untested |
| 17 | **No snapshot crypto verification** | `crates/storage` | `verify_snapshot` counts signatures only; no Ed25519 crypto verify |
| 18 | **No fast sync catch-up** | `crates/storage` | `incremental_sync()` returns `0`; post-snapshot catch-up untested |
| 19 | **No real-network light client tests** | `crates/light-client` | No 7+ day soak against live Ethereum RPC |
| 20 | **No CI/CD pipeline** | Repository | Tests run manually; no automated PR execution or coverage |

---

## Medium (Should Resolve Before Launch)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 21 | **OTel spans not called in production paths** | `crates/node` | `record_block_span()` etc. defined but unused in hot paths |
| 22 | **No Grafana dashboards** | `docs/observability.md` | No pre-built JSON dashboard files |
| 23 | **Structured error codes missing** | `crates/rpc` | Errors are strings; no machine-readable codes |
| 24 | **No WebSocket lag handling tests** | `crates/rpc` | Lagged subscribers silently drop events |
| 25 | **No CORS configuration tests** | `crates/rpc` | Default jsonrpsee CORS untested |
| 26 | **No mempool eviction under pressure tests** | `crates/mempool` | `ReplayProtector` evicts 25% but correctness untested |
| 27 | **No P2P ban enforcement tests** | `crates/network` | Ban duration defined but not verified in practice |
| 28 | **No property-based / fuzz testing** | Entire codebase | No `proptest`, `quickcheck`, or `cargo-fuzz` |
| 29 | **No benchmark suite** | Entire codebase | No `criterion.rs` for hot paths |
| 30 | **No testnet environment** | Infrastructure | E2E runs in-memory only; no soak testing |
| 31 | **Churn limit / safety floor unimplemented** | `crates/consensus` | `docs/validator_staking.md` describes mechanisms not yet in code |

---

## Future (Deferred, Not Blocking)

| # | Item | Scope | Notes |
|---|------|-------|-------|
| 32 | **EthLightClient BLS consensus verification** | `crates/light-client` | Parent-hash chain sufficient for devnet/testnet; beacon chain sync deferred |
| 33 | **Prover key rotation** | `crates/shielded` | Governance-driven ceremony; mainnet readiness only |
| 34 | **MEV protection** | `crates/protocol` | Commit-reveal library exists but not integrated into block production |
| 35 | **System contracts** | Long-term | Protocol logic in Rust precompiles; Solidity migration deferred |

---

## What's Working Well

- **~1,682 tests** across 99 files, spanning unit, integration, and E2E
- **EVM-only architecture**: all state in MDBX (`CallEvmAccounts` / `CallEvmStorage`)
- **Commonware Simplex BFT** consensus with VRF proposer rotation
- **Dynamic gas metering** (`base + sloads*50 + sstores*500`) via revm Journal
- **`StorageRef`** safe journal decomposition (replaced unsafe `JournalBackend`)
- **All precompiles wired** (`0x101` oracle, `0x103` bridge, `0x201` asset, `0x202` shielded, `0x203` governance, `0x204` validator, `0x205` compliance, `0x207` switch, `0x209` agent)
- **Shielded pool** with Groth16 proofs (deposit/transfer/withdraw)
- **Bridge** with optimistic challenge period (initiate/resolve/withdraw bond)
- **Governance** with timelock, auto-advance, and 10 proposal types
- **Fast sync** with Ed25519-signed state snapshots (production wired; cryptographic verification tests incomplete — see #17)
- **Light client** with MPT proof verification and header gossip

---

## Recommended Priority Order

1. **Before public testnet**: CI/CD (#20), TLS/auth tests (#2-3), MDBX integration (#4), oracle signature tests (#8), performance tests (#11-12), slashing tests (#14)
2. **Before mainnet audit kickoff**: All critical items (#1, #5-10), real-network light client soak (#19)
3. **Before mainnet launch**: Implement churn limit / safety floor / dynamic unbonding (#31), benchmark suite (#29), formal shielded verification (#10)

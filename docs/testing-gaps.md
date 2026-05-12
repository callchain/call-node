# Callchain Testing Gaps — Tracking Document

**Updated**: 2026-05-11
**Source**: `docs/testing.md` — extracted unresolved gaps for focused tracking.

---

## How to Use This Document

This file tracks only **open (unresolved) testing gaps**. When a gap is closed:
1. Move it to the "Recently Closed" section at the bottom
2. Reference the commit / PR that resolved it
3. Update `docs/testing.md` accordingly

---

## Critical (Block Mainnet)

> **Note**: RPC authentication / authorization (JWT, API keys) is intentionally out of scope. Callchain nodes are expected to run behind a reverse proxy or VPN where auth is handled at the infrastructure layer.

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| 1 | ~~RPC rate limiting~~ | ~~`call-rpc`~~ | ~~Per-client request throttling, burst handling, ban-after-excess~~ |
| 2 | ~~Concurrent DB access / crash recovery~~ | ~~`call-storage`~~ | ~~Simultaneous writers, WAL replay after kill -9, corruption detection~~ |
| 3 | Light client beacon BLS consensus | `call-light-client` | Mock beacon API → `fetch_light_client_finality_update` → `apply_light_client_update` → `set_finalized_block` |
| 4 | Oracle signature negative tests | `call-oracle` | Submit with invalid 64-byte signature → must fail |

---

## High (Block Public Testnet)

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| 5 | `eth_getLogs` at 1M+ blocks | `call-evm` | Create 1M blocks with receipts, measure scan latency and memory |
| 6 | Receipt DB persistence | `call-protocol` | Write receipts, restart node, verify recovery from DB |
| 7 | Agent tx in block production | `call-node` | Full E2E: agent registers → submits tx → included in block → executed |
| 8 | Agent domain real verification | `call-agent` | DNS TXT record lookup, HTTP file fetch, timeout/failure handling |
| 9 | BatchTransfer permission bypass | `call-protocol` | Multi-payment batch where payment[1+] skips permission check |
| 10 | Slashing economic penalty | `call-consensus` | Double-sign detected → stake reduced → validator removed from set |
| 11 | ForkManager persistence | `call-consensus` | Serialize to DB, restart, scheduled upgrades retained |
| 12 | Multi-upgrade same height | `call-consensus` | Two upgrades at height H → only first applied, second logged |
| 13 | Snapshot production + sig verify | `call-storage` | Trigger snapshot, verify cryptographic signatures (not just count) |
| 14 | Fast sync incremental catch-up | `call-node` | Snapshot at block N, head at N+10K, verify catch-up completes |

---

## Medium (Ongoing)

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| 15 | Agent `credit()` u128 overflow | `call-agent` | `credit()` with `u128::MAX` + 1 → must not wrap |
| 16 | EIP-2718 typed receipt parsing | `call-light-client` | Type 0x01 (EIP-2930) and Type 0x02 (EIP-1559) receipt proofs |
| 17 | Light client reorg handling | `call-light-client` | Feed orphaned headers, verify rollback and resync |
| 18 | WebSocket lag handling | `call-rpc` | Slow subscriber → verify lag notification or silent drop behavior |
| 19 | Compliance report symbol accuracy | `call-node` | Asset ID 2 mapped to correct symbol, not hardcoded "CALL" |
| 20 | Log rotation under load | `call-node` | Rapid 1000 logs/sec, disk-full simulation |
| 21 | P2P ban enforcement | `call-network` | Exceed rate limit → peer banned → reconnect rejected during ban window |
| 22 | Mempool eviction under pressure | `call-mempool` | ReplayProtector over limit → evict 25% → verify no false evictions |
| 23 | Light client bridge deposit E2E | `call-bridge` | Full flow: `call_lightClientBridgeDeposit` with `light-client-bridge` feature |
| 24 | Bridge MPT proof verification | `call-bridge` | Tx inclusion proof + receipt proof against real Ethereum header |
| 25 | Beacon sync background task | `call-node` | `start_beacon_sync_task` tick → fetch → BLS verify → `is_consensus_verified` |
| 26 | OpenTelemetry span in hot path | `call-node` | Block production triggers `record_block_span`, span emitted to collector |
| 27 | FileLogLayer high-volume rotation | `call-node` | Background task handles sustained 10K logs/sec without drop |

---

## Infrastructure (Meta)

| # | Gap | Tool / Approach |
|---|-----|-----------------|
| 28 | CI/CD pipeline | GitHub Actions: `cargo test --workspace` on PR, nightly full suite |
| 29 | Code coverage | `cargo tarpaulin` or `cargo llvm-cov`, gate PRs at >70% |
| 30 | Benchmark suite | `criterion.rs` for MPT verify, proof gen, block production hot paths |
| 31 | Property-based testing | `proptest` for RLP decode, MPT parse, precompile dispatch invariants |
| 32 | Long-running testnet | 7+ day soak test with real Ethereum RPC, mixed validator versions |
| 33 | Mutation testing | `cargo-mutants` to verify test suite actually catches bugs |

---

## Recently Closed

| # | Gap | Resolution | Commit |
|---|-----|-----------|--------|
| — | TLS/HTTPS tests | `tls_integration.rs` (3 tests) | prior |
| — | CORS tests | `cors_integration.rs` (6 tests) | prior |
| — | MDBX integration | `test_mdbx_integration.rs` (35 tests) | prior |
| — | Network partition | `test_network_partition.rs` (7 tests) | prior |
| — | Byzantine faults | `test_byzantine_faults.rs` (4 tests) | prior |
| — | Audit log integrity | Merkle root, file roundtrip, append-only | `06c579b` |
| — | Structured logging | JSON/text, rotation, config defaults | `06c579b` |
| — | Compliance CSV export | CSV generation with dynamic timestamps | `06c579b` |
| — | Oracle E2E | Price submit/read via TestNode | prior |
| — | Light client sync | Ethereum sync committee header verify | prior |
| — | Soak test baseline | Sustained load E2E | prior |
| 1 | RPC rate limiting | `crates/rpc/src/rate_limit.rs` (7 tests) | `1cf0214` |
| 2 | Concurrent DB access / crash recovery | `crates/storage/src/reth_db.rs` (+3 tests: same-key race, reopen persist, WAL checkpoint) | current |

---

## Stats

- **Total open**: 31 gaps (2 critical + 10 high + 13 medium + 6 infrastructure)
- **Recently closed**: 13 gaps
- **Target**: Close all critical + high before mainnet; medium + infra before public testnet

# Callchain Testing Strategy

## Overview

The Callchain test suite spans unit tests (per-crate), integration tests (cross-crate protocol flows), and end-to-end tests (full node lifecycle, multi-node networks). Total test count: **~856 `#[test]` / `#[tokio::test]` annotations across ~92 files**.

**Test philosophy:**
- Unit tests for individual components (consensus, protocol, crypto, storage, etc.)
- Integration tests for cross-crate flows (payment, bridge, governance, agent, shielded, network)
- E2E tests for full node behavior (block production, networking, malicious actors, stress, soak)

---

## Test Matrix

### Unit Tests by Crate

| Crate | Test Count | Coverage Areas |
|-------|-----------|----------------|
| `call-consensus` | ~168 | Block production, BFT rounds, fork choice, validator set, proposer selection, upgrade scheduling, emergency rollback, block cache, digest |
| `call-protocol` | ~280 | Balances, transfers, batch transfers, fees, allowances, receipts, memos, asset registry, issuer, precompiles, transactions, compliance, sponsor, smart accounts, security limits, mempool defense |
| `call-crypto` | ~40 | keccak256, Ed25519 sign/verify, secp256k1 recovery, BLS, hash functions, keystore |
| `call-evm` | ~29 | EVM state, executor, DB adapter, contract creation, call |
| `call-network` | ~78 | P2P message handling, gossip, peer limits, identity, limits validation |
| `call-storage` | ~46 | Pruning, snapshots, node modes, table descriptors, expiration |
| `call-rpc` | ~33 | Module building, subscription registration, handler state |
| `call-bridge` | ~35 | Deposit flow, external tracking, challenge period, withdrawal, permissionless challenge revocation |
| `call-shielded` | ~160 | Circuit deposit/transfer/withdraw, Merkle tree, Poseidon hash, notes, nullifiers, proof serialization, keygen, compliance |
| `call-light-client` | ~26 | MPT proof verification (leaf, extension, branch, tampered hash), header chain submission, compact encoding |
| `call-agent` | ~105 | Registration, permissions, balances, nonces, precompile call extraction, transaction verification, execution |
| `call-governance` | ~51 | Proposal lifecycle, voting, execution, delegation, timelock |
| `call-oracle` | ~22 | Price submission, aggregation, validator info |
| `call-mempool` | ~46 | Pool ordering, priority, eviction, duplicate handling |
| `call-payload-builder` | ~15 | Block construction, gas accounting, transaction selection |
| `call-node` | ~147 | Telemetry, logging, light client, config, boot |
| `call-precompile` | ~53 | Asset, Oracle, Bridge, Switch, Shielded, Governance, Validator, Compliance, Agent, state hook lifecycle |
| `call-chainspec` | ~25 | Genesis configuration, validator initialization |
| `call-serialization` | ~12 | JSON, RLP encoding/decoding |
| `call-primitives` | ~20 | Address, Hash, BlockHash, AssetId operations |

### Integration Tests

| Test File | Coverage | Location |
|-----------|----------|----------|
| `test_shielded_flow.rs` | Deposit, transfer, withdrawal, note management, nullifier tracking | `crates/shielded/tests/` |
| `test_mdbx_integration.rs` | 22 CallTables put/get/delete roundtrips, batch writes, iteration, overwrite | `crates/storage/tests/` |
| `integration_test.rs` | P2P message handling, gossip propagation, peer connection limits | `crates/network/tests/` |
| `eth_sync_e2e.rs` | Light client Ethereum sync committee header verification | `crates/light-client/tests/` |
| `tls_integration.rs` | TLS handshake, certificate validation, HTTPS RPC | `crates/rpc/tests/` |
| `cors_integration.rs` | CORS preflight, allowed origins, header handling | `crates/rpc/tests/` |

### E2E Tests (`crates/node/tests/`)

| Test File | Coverage |
|-----------|----------|
| `test_consensus_block_production.rs` | Single-node block production, validator staking, proposer subset, transaction inclusion |
| `test_full_node_lifecycle.rs` | Node startup, shutdown, config loading, state initialization |
| `test_evm_compatibility.rs` | EVM transaction execution, state updates, receipt generation |
| `test_shielded_e2e.rs` | Full shielded deposit/transfer/withdraw via node harness |
| `test_malicious_proposer.rs` | Double-sign slashing, offline penalty, invalid tx block rejection, double nonce rejection |
| `test_stress.rs` | High throughput (1000 txs), mempool capacity, base fee response, double-spend prevention, state consistency, multi-sender stress, rapid block production (500 blocks) |
| `test_fork_upgrade.rs` | Height-activated upgrade, chain fork reconcile, governance-triggered upgrade |
| `test_multi_node_network.rs` | Two-node block propagation, transaction propagation, multi-node consensus |
| `test_governance_e2e.rs` | Full proposal lifecycle (submit → vote → queue → execute) via `TestNode` harness |
| `test_bridge_e2e.rs` | Bridge deposit, withdrawal, and insufficient-signature rejection via `TestNode` harness |
| `test_light_client_e2e.rs` | Light client block header verification (`call_lightVerifyBlockHeader`) and balance proofs (`call_lightGetBalanceProof`) via `TestNode` harness |
| `test_websocket_e2e.rs` | All 9 WebSocket subscription channels |
| `test_oracle_e2e.rs` | Oracle price submission and retrieval via `TestNode` harness |
| `test_network_partition.rs` | Partition groups block cross-group messages, healing restores communication, partial drops |
| `test_byzantine_faults.rs` | Equivocation, withholding, message flood via `PartitionSimulator` |
| `test_soak.rs` | Sustained load over extended period |

---

## Production Readiness Gaps

### Critical Gaps

| # | Gap | Impact | Status |
|---|-----|--------|--------|
| 1 | ~~**No TLS/HTTPS tests**~~ | ~~RPC servers bind to plain HTTP~~ | **RESOLVED** — `crates/rpc/tests/tls_integration.rs` (3 tests) |
| 2 | **No authentication/authorization tests** | No API key, JWT, or IP allowlist tests. All RPC endpoints are effectively unprotected in tests and production. | Open |
| 3 | **No rate limiting tests for RPC** | `max_connections` caps concurrent connections but no tests verify per-client request throttling. | Open |
| 4 | ~~**No MDBX read/write tests**~~ | ~~Storage crate has table descriptors but no actual MDBX integration~~ | **RESOLVED** — `crates/storage/tests/mdbx_integration.rs` (35 tests) |
| 5 | **No concurrent access/corruption recovery tests** | No tests for concurrent DB writes, crash recovery, or WAL behavior. | Open |
| 6 | ~~**No network partition tests**~~ | ~~E2E tests use local harness~~ | **RESOLVED** — `crates/node/tests/test_network_partition.rs` (7 tests) |
| 7 | **No light client beacon BLS consensus verification tests** | `apply_light_client_update` and `bls_verify_aggregate_beacon` exist but no E2E test verifies beacon sync committee signature validation against live or mock beacon API. | Open |
| 8 | **No oracle signature verification negative tests** | Oracle price submissions accept any 64-byte signature. No negative test exists with invalid signature data. | Partial — `test_oracle_e2e.rs` covers positive path only |

### High Gaps

| # | Gap | Details | Status |
|---|-----|---------|--------|
| 9 | **No `eth_getLogs` performance tests** | Scans all receipts linearly (O(n)). No test validates behavior at 1M+ blocks. | Open |
| 10 | **No receipt persistence tests** | Receipts are in-memory only. No test verifies DB persistence or recovery. | Open |
| 11 | **No agent integration with block production tests** | Agent transactions exist as a library but are not executed during consensus. No E2E test covers agent tx in a block. | Open |
| 12 | **No domain verification tests (real DNS/HTTP)** | Agent domain verification is format-only. No tests with actual DNS TXT or HTTP file verification. | Open |
| 13 | **No batch transfer multi-payment permission tests** | `BatchTransfer` only checks the first payment. No test covers subsequent payments bypassing permission checks. | Open |
| 14 | **No slashing economic penalty tests** | Double-sign detection works but no test verifies stake reduction or validator removal. | Open |
| 15 | **No upgrade persistence tests** | ForkManager is in-memory only. No test verifies scheduled upgrades survive restart. | Open |
| 16 | **No multi-upgrade-at-same-height tests** | `check_upgrades_at_height` applies only the first match. No test catches this bug. | Open |
| 17 | **No snapshot production/verification tests** | Snapshot production is not wired. `verify_snapshot` does not cryptographically verify signatures. Tests only count signatures. | Open |
| 18 | **No fast sync incremental catch-up tests** | `incremental_sync()` returns `Ok(0)`. No test verifies catch-up from snapshot to chain head. | Open |

### Medium Gaps

| # | Gap | Details | Status |
|---|-----|---------|--------|
| 19 | **No overflow tests for agent balance credit** | `credit()` uses naive addition. No test for u128 overflow wrapping. | Open |
| 20 | **No typed receipt (EIP-2718) parsing tests** | Light client assumes legacy receipt format. No tests for Type 0x01/0x02 receipts. | Open |
| 21 | **No reorg handling tests for light client** | Orphaned headers are never removed. No test for following wrong chain. | Open |
| 22 | **No WebSocket lag handling tests** | Lagged subscribers are not notified. No test verifies silent event dropping. | Open |
| 23 | ~~**No CORS configuration tests**~~ | ~~Default jsonrpsee CORS policy untested~~ | **RESOLVED** — `crates/rpc/tests/cors_integration.rs` (6 tests) |
| 24 | **No compliance report accuracy tests** | `export_compliance_report` uses hardcoded asset symbol mapping. No test validates symbol correctness across asset IDs. | Partial — basic CSV format tested in `crates/node/src/logging.rs` |
| 25 | **No log rotation under load tests** | `rotate_log()` renames files sequentially. No test for rapid rotation or disk-full conditions. | Partial — `test_log_rotation` and `test_should_rotate_by_size` exist but no load/disk-full coverage |
| 26 | **No P2P ban enforcement tests** | `NetworkLimits` defines ban duration but no test verifies peer banning works in practice. | Open |
| 27 | **No mempool eviction under memory pressure tests** | `ReplayProtector` evicts 25% when over limit but no test verifies correctness during eviction. | Open |
| 28 | **No cross-crate integration test for light client bridge deposit** | `call_lightClientBridgeDeposit` is feature-gated. No integration test covers the full flow. | Open |
| 29 | **No MPT proof verification tests** | Bridge MPT proof verification is behind `light-client-bridge` feature flag. No tests validate tx inclusion or receipt proof verification against Ethereum headers. | Open |
| 30 | **No beacon sync background task tests** | `start_beacon_sync_task` fetches `LightClientUpdate` periodically. No test verifies fetch → BLS verify → `set_finalized_block` flow. | Open |
| 31 | **No OpenTelemetry span production tests** | `record_block_span`, `record_tx_span`, `record_p2p_span` exist but are not called in hot paths. No test verifies they fire during real block production. | Open |
| 32 | **No FileLogLayer rotation under load tests** | Background file logger task checks rotation every 60s. No test verifies behavior under sustained high log volume. | Open |

---

## Remaining Production Gaps

| # | Gap | Scope |
|---|-----|-------|
| 1 | ~~**Byzantine consensus tests**~~: network partitions, equivocation, delayed messages | ~~`crates/node/tests/`~~ — **RESOLVED** by `test_byzantine_faults.rs` and `test_network_partition.rs` |
| 2 | **Load test**: sustained 1000 TPS for 1 hour + memory profiling | `crates/node/tests/` — Partial: `test_soak.rs` exists but may not cover 1-hour sustained load |

---

## Recommended Test Additions

### Phase 1 — Critical (Before Mainnet)

1. **RPC security tests**: Add tests for JWT auth, rate limiting.
2. **MDBX integration tests**: ✅ **Done** — `crates/storage/tests/mdbx_integration.rs`
3. **Byzantine consensus tests**: ✅ **Done** — `test_byzantine_faults.rs`, `test_network_partition.rs`
4. **Oracle signature verification negative tests**: Add invalid signature rejection tests.
5. **Light client beacon sync E2E**: Mock beacon API server + verify BLS signature validation in `apply_light_client_update`.
6. **OTel span integration tests**: Wire `record_block_span` into block production and verify span emission.

### Phase 2 — High (Before Public Testnet)

1. **Performance tests**: `eth_getLogs` at 100K blocks, mempool at 10K txs, block production at max size.
2. **Agent block production integration**: Full E2E test where an agent transaction is included in a block.
3. **Slashing penalty tests**: Verify stake reduction and validator set removal after double-sign.
4. **Upgrade persistence tests**: Serialize ForkManager, restart, verify scheduled upgrades retained.
5. **Light client real receipt tests**: Test with actual Ethereum receipt proofs using correct index keys.

### Phase 3 — Medium (Ongoing)

1. **Fuzz tests**: Transaction RLP decoding, MPT proof parsing, precompile call deserialization.
2. **Chaos tests**: Random node restarts, network delays, message drops.
3. **Load tests**: Sustained 1000 TPS for 1 hour, memory profiling. Validate on real hardware with cross-region latency (50-200ms) and packet loss simulation.
4. **Real-network light client tests**: Connect to live Ethereum RPC for 7+ days. Verify header chain submission, receipt proofs, reorg handling.
5. **Heterogeneous fork upgrade tests**: Mixed-version testnet (50% old / 50% new). Verify upgrade activation, backward/forward compatibility, no consensus split.
6. **Audit log integrity tests**: ✅ **Done** — `test_audit_log_merkle_root`, `test_audit_log_file_roundtrip` in `crates/node/src/logging.rs`.

---

## Test Infrastructure Gaps

| # | Gap | Severity |
|---|-----|----------|
| 1 | **No CI/CD pipeline configuration** | Tests are run manually. No automated test execution on PRs. |
| 2 | **No code coverage tracking** | No `cargo tarpaulin` or `llvm-cov` integration. Unknown actual coverage percentage. |
| 3 | **No benchmark suite** | No `criterion.rs` benchmarks for hot paths (MPT verification, proof generation, block production). E2E `test_stress.rs` runs in-memory; no real-network latency/packet-loss validation. |
| 4 | **No property-based testing** | No `proptest` or `quickcheck` for invariant-based testing. |
| 5 | **No testnet environment** | E2E tests run in-memory. No long-running testnet for soak testing, light client real-network validation, or heterogeneous fork upgrade testing. |
| 6 | **No mutation testing** | No `cargo-mutants` to verify test suite effectiveness. |

---

## Recently Resolved Gaps

| Gap | Resolution | File |
|-----|-----------|------|
| TLS/HTTPS tests | `tls_integration.rs` added | `crates/rpc/tests/tls_integration.rs` |
| CORS tests | `cors_integration.rs` added | `crates/rpc/tests/cors_integration.rs` |
| MDBX integration | `test_mdbx_integration.rs` with 35 table tests | `crates/storage/tests/mdbx_integration.rs` |
| Network partition | `test_network_partition.rs` with 7 partition scenarios | `crates/node/tests/test_network_partition.rs` |
| Byzantine faults | `test_byzantine_faults.rs` with equivocation/withholding/flood | `crates/node/tests/test_byzantine_faults.rs` |
| Audit log integrity | Merkle root determinism, file roundtrip, append-only | `crates/node/src/logging.rs` |
| Structured logging | JSON/text format, rotation, config defaults | `crates/node/src/logging.rs` |
| Compliance export | CSV generation with dynamic timestamps | `crates/node/src/logging.rs` |
| Oracle E2E | Price submit/read via TestNode harness | `crates/node/tests/test_oracle_e2e.rs` |
| Light client sync | Ethereum sync committee header verification | `crates/light-client/tests/eth_sync_e2e.rs` |
| Soak test | Sustained load baseline | `crates/node/tests/test_soak.rs` |
| Network integration | P2P gossip, peer limits | `crates/network/tests/integration_test.rs` |

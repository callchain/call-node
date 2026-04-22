# Callchain Testing Strategy

## Overview

The Callchain test suite spans unit tests (per-crate), integration tests (cross-crate protocol flows), and end-to-end tests (full node lifecycle, multi-node networks). Total test count: ~1,682 test annotations across 99 files.

**Test philosophy:**
- Unit tests for individual components (consensus, protocol, crypto, storage, etc.)
- Integration tests for cross-crate flows (payment, bridge, governance, agent, shielded)
- E2E tests for full node behavior (block production, networking, malicious actors, stress)

---

## Test Matrix

### Unit Tests by Crate

| Crate | Test Count | Coverage Areas |
|-------|-----------|----------------|
| `call-consensus` | ~168 | Block production, BFT rounds, fork choice, validator set, proposer selection, upgrade scheduling, emergency rollback, block cache, digest |
| `call-protocol` | ~280 | Balances, transfers, batch transfers, fees, allowances, receipts, memos, asset registry, issuer, instructions, transactions, compliance, sponsor, smart accounts, security limits, mempool defense |
| `call-crypto` | ~40 | keccak256, Ed25519 sign/verify, secp256k1 recovery, BLS, hash functions, keystore |
| `call-evm` | ~29 | EVM state, executor, DB adapter, contract creation, call |
| `call-network` | ~78 | P2P message handling, gossip, peer limits, identity, limits validation |
| `call-storage` | ~46 | Pruning, snapshots, node modes, table descriptors, expiration |
| `call-rpc` | ~33 | Module building, subscription registration, handler state |
| `call-bridge` | ~35 | Deposit flow, external tracking, challenge period, withdrawal, permissionless challenge revocation |
| `call-shielded` | ~160 | Circuit deposit/transfer/withdraw, Merkle tree, Poseidon hash, notes, nullifiers, proof serialization, keygen, compliance |
| `call-light-client` | ~26 | MPT proof verification (leaf, extension, branch, tampered hash), header chain submission, compact encoding |
| `call-agent` | ~105 | Registration, permissions, balances, nonces, instruction extraction, transaction verification, execution |
| `call-governance` | ~51 | Proposal lifecycle, voting, execution, delegation, timelock |
| `call-oracle` | ~22 | Price submission, aggregation, validator info |
| `call-transaction-pool` | ~46 | Pool ordering, priority, eviction, duplicate handling |
| `call-payload-builder` | ~15 | Block construction, gas accounting, transaction selection |
| `call-node` | ~147 | Telemetry, logging, light client, config, boot |
| `call-precompiles` | ~28 | Balance read, oracle read, bridge precompile |
| `call-chainspec` | ~25 | Genesis configuration, validator initialization |
| `call-serialization` | ~12 | JSON, RLP encoding/decoding |
| `call-primitives` | ~20 | Address, Hash, BlockHash, AssetId operations |

### Integration Tests (`crates/protocol/tests/`)

| Test File | Coverage |
|-----------|----------|
| `test_payment_flow.rs` | Transfer, batch transfer, fee deduction, insufficient balance, memo, allowance |
| `test_bridge_flow.rs` | Deposit, external tracking, challenge period, completion, withdrawal, permissionless challenge revocation |
| `test_governance_flow.rs` | Proposal creation, voting, execution, timelock, delegation |
| `test_agent_flow.rs` | Registration, permission checks, balance operations, transaction execution, nonce tracking |
| `test_shielded_flow.rs` | Deposit, transfer, withdrawal, note management, nullifier tracking |
| `test_shielded_integration.rs` | End-to-end shielded lifecycle with Merkle tree updates |

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
| `test_websocket_e2e.rs` | All 9 WebSocket subscription channels (`call_subscribeNewBlocks`, `call_subscribeNewPayments`, `call_subscribeBridgeCompleted`, `call_subscribeAssetRegistered`, `call_subscribeAgentExecuted`, `call_subscribeAgentRevoked`, `call_subscribeShieldedDeposit`, `call_subscribeShieldedWithdrawal`, `call_subscribeGovernance`) |

---

## Production Readiness Gaps

### Critical Gaps

| # | Gap | Impact |
|---|-----|--------|
| 1 | **No TLS/HTTPS tests** | RPC servers bind to plain HTTP. No tests verify TLS termination or certificate handling. |
| 2 | **No authentication/authorization tests** | No API key, JWT, or IP allowlist tests. All RPC endpoints are effectively unprotected in tests and production. |
| 3 | **No rate limiting tests for RPC** | `max_connections` caps concurrent connections but no tests verify per-client request throttling. |
| 4 | **No MDBX read/write tests** | Storage crate has table descriptors but no actual MDBX integration tests. Production would run on JSON fallback. |
| 5 | **No concurrent access/corruption recovery tests** | No tests for concurrent DB writes, crash recovery, or WAL behavior. |
| 6 | **No network partition tests** | E2E tests use local harness. No tests for network partitions, Byzantine nodes, or message delays. |
| 7 | **No light client consensus verification tests** | Light client does not verify Ethereum BLS signatures. No tests for malicious fork feeding. |
| 8 | **No oracle signature verification tests** | Oracle price submissions accept any 64-byte signature. No negative test exists. |

### High Gaps

| # | Gap | Details |
|---|-----|---------|
| 9 | **No `eth_getLogs` performance tests** | Scans all receipts linearly (O(n)). No test validates behavior at 1M+ blocks. |
| 10 | **No receipt persistence tests** | Receipts are in-memory only. No test verifies DB persistence or recovery. |
| 11 | **No agent integration with block production tests** | Agent transactions exist as a library but are not executed during consensus. No E2E test covers agent tx in a block. |
| 12 | **No domain verification tests (real DNS/HTTP)** | Agent domain verification is format-only. No tests with actual DNS TXT or HTTP file verification. |
| 13 | **No batch transfer multi-payment permission tests** | `BatchTransfer` only checks the first payment. No test covers subsequent payments bypassing permission checks. |
| 14 | **No slashing economic penalty tests** | Double-sign detection works but no test verifies stake reduction or validator removal. |
| 15 | **No upgrade persistence tests** | ForkManager is in-memory only. No test verifies scheduled upgrades survive restart. |
| 16 | **No multi-upgrade-at-same-height tests** | `check_upgrades_at_height` applies only the first match. No test catches this bug. |
| 17 | **No snapshot production/verification tests** | Snapshot production is not wired. `verify_snapshot` does not cryptographically verify signatures. Tests only count signatures. |
| 18 | **No fast sync incremental catch-up tests** | `incremental_sync()` returns `Ok(0)`. No test verifies catch-up from snapshot to chain head. |

### Medium Gaps

| # | Gap | Details |
|---|-----|---------|
| 19 | **No overflow tests for agent balance credit** | `credit()` uses naive addition. No test for u128 overflow wrapping. |
| 20 | **No typed receipt (EIP-2718) parsing tests** | Light client assumes legacy receipt format. No tests for Type 0x01/0x02 receipts. |
| 21 | **No reorg handling tests for light client** | Orphaned headers are never removed. No test for following wrong chain. |
| 22 | **No WebSocket lag handling tests** | Lagged subscribers are not notified. No test verifies silent event dropping. |
| 23 | **No CORS configuration tests** | Default jsonrpsee CORS policy untested. |
| 24 | **No compliance report accuracy tests** | `export_compliance_report` uses hardcoded asset symbol "CALL" and may produce incorrect timestamps. |
| 25 | **No log rotation under load tests** | `rotate_log()` renames files sequentially. No test for rapid rotation or disk-full conditions. |
| 26 | **No P2P ban enforcement tests** | `NetworkLimits` defines ban duration but no test verifies peer banning works in practice. |
| 27 | **No mempool eviction under memory pressure tests** | `ReplayProtector` evicts 25% when over limit but no test verifies correctness during eviction. |
| 28 | **No cross-crate integration test for light client bridge deposit** | `call_lightClientBridgeDeposit` is feature-gated. No integration test covers the full flow. |
| 29 | **No MPT proof verification tests** | Bridge MPT proof verification is behind `light-client-bridge` feature flag. No tests validate tx inclusion or receipt proof verification against Ethereum headers. |

---

## Remaining Production Gaps

| # | Gap | Scope |
|---|-----|-------|
| 1 | **Byzantine consensus tests**: network partitions, equivocation, delayed messages | `crates/node/tests/` |
| 2 | **Load test**: sustained 1000 TPS for 1 hour + memory profiling | `crates/node/tests/` |

---

## Recommended Test Additions

### Phase 1 — Critical (Before Mainnet)

1. **RPC security tests**: Add tests for JWT auth, rate limiting, and TLS handshake.
2. **MDBX integration tests**: Create a test MDBX env, write/read/delete data, verify transactions.
3. **Byzantine consensus tests**: Network partition scenarios, malicious proposer withholding blocks, equivocation.
4. **Oracle signature verification tests**: Negative tests with invalid signatures must fail.

### Phase 2 — High (Before Public Testnet)

1. **Performance tests**: `eth_getLogs` at 100K blocks, mempool at 10K txs, block production at max size.
2. **Agent block production integration**: Full E2E test where an agent transaction is included in a block.
3. **Slashing penalty tests**: Verify stake reduction and validator set removal after double-sign.
4. **Upgrade persistence tests**: Serialize ForkManager, restart, verify scheduled upgrades retained.
5. **Light client real receipt tests**: Test with actual Ethereum receipt proofs using correct index keys.

### Phase 3 — Medium (Ongoing)

1. **Fuzz tests**: Transaction RLP decoding, MPT proof parsing, instruction deserialization.
2. **Chaos tests**: Random node restarts, network delays, message drops.
3. **Load tests**: Sustained 1000 TPS for 1 hour, memory profiling. Validate on real hardware with cross-region latency (50-200ms) and packet loss simulation.
4. **Real-network light client tests**: Connect to live Ethereum RPC for 7+ days. Verify header chain submission, receipt proofs, reorg handling.
5. **Heterogeneous fork upgrade tests**: Mixed-version testnet (50% old / 50% new). Verify upgrade activation, backward/forward compatibility, no consensus split.
6. **Audit log integrity tests**: Tamper detection, Merkle proof verification for audit entries.

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

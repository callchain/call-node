# Callchain Security & Threat Model

## Overview

The Security layer (`crates/protocol/src/security.rs`, `crates/network/src/limits.rs`) provides block limits, mempool attack prevention, shielded pool defense, P2P rate limiting, consensus double-sign detection, and MEV protection primitives. Most critical gaps identified in earlier audits have been resolved; a small number remain open.

**Security model assumptions:**
- Honest majority of validators (2/3+ for BFT safety)
- Ed25519 signatures for consensus messages
- secp256k1 signatures for protocol transactions and EVM transactions
- Network peers authenticated via libp2p Noise + PeerId

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Security Controls                                           │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Block Limits       │  │ Mempool Defense              │  │
│  │ - max_tx_size      │  │ - rate_limiter               │  │
│  │ - max_instructions │  │ - replay_protector           │  │
│  │ - max_batch_size   │  │ - address_saturation         │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Shielded Defense   │  │ Consensus Defense            │  │
│  │ - per-block limit  │  │ - double-sign detection      │  │
│  │ - nullifier set    │  │ - slashing tracker           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ P2P Defense        │  │ MEV Protection               │  │
│  │ - peer rate limits │  │ - commit-reveal              │  │
│  │ - msg size caps    │  │ - builder registry           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Cross-Cutting Gaps (RPC auth, domain verification,      ││
│  │ mempool defense wiring)                                 ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Block Limits (`security.rs`)

`BlockLimits` enforces:
- Max tx size: 64 KB
- Max instructions per tx: 256
- Max batch recipients: 100
- Max shielded proofs per block: 50
- Max txs per block: 10,000
- Max block size: 4 MB

**Production ready:** Yes. The struct is well-defined and tested. Called in `Block::execute()` before processing transactions.

### 2. Mempool Defense (`security.rs`)

`MempoolDefense` combines:
- `RateLimiter`: per-address sliding window (configurable max requests / window ms)
- `ReplayProtector`: bounded HashSet of seen tx hashes, evicts ~25% when over limit
- Address saturation: max txs per address per window

**Production ready:** Partial. The library works and is tested, but it is not integrated into the RPC transaction submission path (`call_sendPayment`, `eth_sendRawTransaction`). The RPC layer accepts transactions without rate limiting.

### 3. Shielded Pool Defense (`security.rs`)

`ShieldedDefense` enforces:
- Per-block shielded tx limit
- Global nullifier set for double-spend prevention

**Production ready:** Yes. The nullifier set persists to the `CallShieldedNullifiers` column family on every block save and reloads on node startup (`crates/node/src/lib.rs:1192-1240`).

### 4. P2P Defense (`security.rs`)

`P2PDefense` provides:
- Per-peer message rate limiting
- Maximum message size enforcement

**Production ready:** Yes. Wired into the P2P receive loop at `crates/node/src/lib.rs:305-316`. `NetworkLimits` in `limits.rs` defines defaults (50 peers, 100 msg/sec, 10 MB max) and actual enforcement is active.

### 5. Consensus Defense (`security.rs`)

`ConsensusDefense` detects double-signing:
- Tracks `(round, validator_id) -> block_hash` mappings
- Returns `DoubleSignEvidence` when a validator signs two different blocks at the same round
- Maintains `slashed_validators` HashSet

**Production ready:** Yes. Detection logic is correct and integrated into `SimplexConsensus`. Slashing removes validators from the active set and burns self-stake:
- `slash_double_sign`: full self-stake slashed, validator removed
- `slash_offline`: proportional slash (rounds * rate% of self_stake)
- `slash_oracle_outlier`: 0.1% self-stake slash

### 6. MEV Protection (`security.rs`)

`MevProtection` implements commit-reveal for Proposer-Builder Separation:
- `commit_tx()`: stores commitment hash
- `reveal_tx()`: verifies commitment matches revealed data via keccak256

**Production ready:** Not ready. The PBS system exists as a library but is not integrated into block production. No builder registration flow is wired.

---

## Threat Model & Cross-Module Gaps

### Critical Severity

| # | Gap | Module | Status | Details |
|---|-----|--------|--------|---------|
| 1 | Protocol instruction signatures not verified | Protocol | **Fixed** | `tx.verify_signature_with_registry()` is called in `Block::execute()` at `crates/consensus/src/block.rs:315` before executing any protocol transactions. RPC `insert_protocol_tx` also verifies at `handlers.rs:687`. |
| 4 | Bridge deposit signatures not verified | RPC/Bridge | **Fixed** | `verify_bridge_signatures()` in `crates/bridge/src/external.rs:175-230` performs real secp256k1 recovery and checks validator set membership, rejecting duplicate validators. |
| 5 | Light client block header signatures not verified | RPC/Light | **Fixed** | `call_lightVerifyBlockHeader` verifies Ed25519 signatures against validator pubkeys (`crates/rpc/src/callchain.rs:679-737`). |
| 6 | Light client balance proofs are fake | RPC/Light | **Fixed** | `call_lightGetBalanceProof` uses the real Merkle tree: `shielded.merkle_tree.proof_for_index(i)` (`crates/rpc/src/callchain.rs:742-780`). |
| 7 | Storage snapshot signatures not verified | Storage | **Fixed** | `verify_snapshot()` performs real Ed25519 verification with fallback (`crates/storage/src/prune.rs:544-567`). |
| 8 | Agent grant does not deduct from owner | Agent | **Fixed** | `grant_funds()` calls `protocol_balances.deduct_balance()` before crediting the agent (`crates/agent/src/balances.rs:83-95`). |

### High Severity

| # | Gap | Module | Status | Details |
|---|-----|--------|--------|---------|
| 9 | ~~RPC has no TLS/HTTPS~~ | RPC | **Fixed** | Both HTTP and WS servers support TLS via `tokio-rustls`. Configured via `tls_cert_path` / `tls_key_path`. |
| 10 | ~~RPC has no rate limiting~~ | RPC | **Fixed** | Per-IP sliding-window rate limiter at connection level (`RateLimiter`). Configurable via `rate_limit_rps` / `rate_limit_window_secs`. |
| 11 | Default agent permissions are wide open | Agent | **Fixed** | `allowed_assets: vec![1]` (only CALL by default), `daily_limit: 10_000`, `per_tx_limit: 1_000` (`crates/agent/src/permissions.rs:23-33`). |
| 12 | Domain verification is format-only | Agent | **Open** | `RealDomainVerifier` exists with live DNS TXT and HTTP file lookups (`crates/agent/src/registry.rs:272-279`), but no production code instantiates it. The default `verify_domain_proof()` only validates format. |
| 13 | Governance execute signature binding broken | RPC/Gov | **Partially addressed** | `insert_protocol_tx` verifies signatures against the actual sender address (fixed). However, `execute_proposal()` has no authorization check on the executor — any account can execute a queued proposal after the timelock expires. |
| 14 | Rollback signatures are replayable | Upgrade | **Fixed** | Per-validator nonce map prevents replay in `submit_rollback_signature()` (`crates/consensus/src/fork.rs:282-397`). |
| 15 | Emergency rollback not executed | Upgrade | **Fixed** | `execute_rollback()` applies the rollback plan when quorum is reached. |
| 16 | Shielded nullifiers are in-memory only | Shielded | **Fixed** | `save_shielded_state_inner` / `load_shielded_state_inner` persist to `CallShieldedNullifiers` DB CF on every block (`crates/node/src/lib.rs:1192-1240`). |
| 17 | No network-wide upgrade sync | Upgrade | **Fixed** | `UpgradeAnnouncement` is broadcast via P2P; receiving nodes auto-schedule the upgrade if they don't already have it pending (`crates/node/src/lib.rs:2961-2984`). |

### Medium Severity

| # | Gap | Module | Status | Details |
|---|-----|--------|--------|---------|
| 18 | Mempool defense not wired to RPC | Security | **Open** | `MempoolDefense` only used in unit tests. RPC endpoints (`call_sendPayment`, `eth_sendRawTransaction`) do not invoke it. |
| 19 | P2P defense not wired to network layer | Security | **Fixed** | `P2PDefense::validate_message()` is called in the P2P receive loop at `crates/node/src/lib.rs:305-316`. |
| 20 | Slashing does not reduce stake | Consensus | **Fixed** | `slash_offline`/`slash_oracle_outlier` reduce `self_stake` and `staked_call`. `slash_double_sign` removes the validator entirely (`crates/consensus/src/validator.rs:296-324`). |
| 21 | No agent revocation/removal | Agent | **Fixed** | `unregister_agent()` exists and removes the agent from the registry (`crates/agent/src/registry.rs`). |
| 22 | Asset registration unpermissioned | RPC/Protocol | **Open** | Fee is charged from issuer balance, but `call_registerAsset` requires no signature proving the caller controls the issuer address. |
| 23 | No registration fee or stake for agents | Agent | **Fixed** | `register_agent()` deducts `registration_fee` from owner balance (`crates/agent/src/registry.rs:113-183`). |
| 24 | Payment signature scheme is non-standard | RPC | **Fixed** | `call_sendPayment` uses EIP-191 `\x19Ethereum Signed Message:\n32` prefix (`crates/rpc/src/handlers.rs:596-606`). |

---

## File Map

| File | Role |
|------|------|
| `crates/protocol/src/security.rs` | Block limits, mempool defense, shielded defense, P2P defense, consensus defense, MEV protection |
| `crates/network/src/limits.rs` | `NetworkLimits`, `NetworkError` — P2P connection/message limits |
| `crates/protocol/src/transaction.rs` | Transaction signing and auth schemes |
| `crates/crypto/src/signer.rs` | Signature verification primitives |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Block limits | Ready | Well-defined, tested, enforced in `Block::execute()` |
| Mempool defense | Partial | Library works, not wired to RPC submission path |
| Shielded defense | Ready | Nullifier tracking works, persists to DB |
| P2P defense | Ready | Wired into P2P receive loop with rate limits + message caps |
| Consensus defense (double-sign) | Ready | Detection + slashing integrated; stake reduction active |
| MEV protection | Not ready | Commit-reveal library exists, not integrated |
| RPC security | Partial | TLS + rate limiting + tx signatures implemented; general API auth (JWT/API key) still missing |
| Signature verification (protocol) | Ready | Verified in consensus block execution and RPC insertion |
| Signature verification (oracle) | Partial | Oracle submissions verified if submitted via protocol tx path |
| Signature verification (bridge) | Ready | Real secp256k1 recovery + validator set check |
| Signature verification (light client) | Ready | Ed25519 signature verification against validator set |
| Signature verification (snapshot) | Ready | Ed25519 verification with fallback |

---

## Test Status

- `cargo test -p call-protocol` (security tests) — covers block limits, mempool rate limiting, address saturation, replay protection, shielded per-block limits, nullifier double-spend, P2P rate limiting, consensus double-sign detection
- `cargo test -p call-network` (limits tests) — covers default limits, custom limits, validation bounds
- `cargo test -p call-consensus` — covers slashing economics, double-sign removal, offline proportional slash
- `cargo test -p call-agent` — covers permission defaults, grant deduction, registration fee, revocation
- Missing: integration of `MempoolDefense` into actual RPC paths, `RealDomainVerifier` integration tests, governance executor authorization tests

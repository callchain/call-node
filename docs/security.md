# Callchain Security & Threat Model

## Overview

The Security layer (`crates/protocol/src/security.rs`, `crates/network/src/limits.rs`) provides block limits, mempool attack prevention, shielded pool defense, P2P rate limiting, consensus double-sign detection, and MEV protection primitives. All gaps identified in earlier audits have been resolved.

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
│  │ - max_precompiles  │  │ - replay_protector           │  │
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
│  │ P2P Defense        │  │ MEV Protection (future)      │  │
│  │ - peer rate limits │  │ - commit-reveal              │  │
│  │ - msg size caps    │  │ - builder registry           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Block Limits (`security.rs`)

`BlockLimits` enforces:
- Max tx size: 64 KB
- Max precompile calls per tx: 256
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

**Production ready:** Yes. Integrated into `RpcState::submit_payment()` and `RpcState::submit_evm_tx()` before mempool insertion. Enforces per-address rate limiting, replay protection, and address saturation.

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

---

## Future Features

### MEV Protection (`security.rs`)

`MevProtection` implements commit-reveal for Proposer-Builder Separation:
- `commit_tx()`: stores commitment hash
- `reveal_tx()`: verifies commitment matches revealed data via keccak256

**Status:** Library exists but not integrated into block production. No builder registration flow is wired. Planned for a future release.

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
| Mempool defense | Ready | Wired into `submit_payment` and `submit_evm_tx` with rate limiting, replay protection, and address saturation |
| Shielded defense | Ready | Nullifier tracking works, persists to DB |
| P2P defense | Ready | Wired into P2P receive loop with rate limits + message caps |
| Consensus defense (double-sign) | Ready | Detection + slashing integrated; stake reduction active |
| RPC security | Ready | TLS + rate limiting + tx signatures + asset registration signatures implemented |
| Signature verification (protocol) | Ready | Verified in consensus block execution and RPC insertion |
| Signature verification (oracle) | Partial | Oracle submissions verified if submitted via protocol tx path |
| Signature verification (bridge) | Ready | Real secp256k1 recovery + validator set check |
| Signature verification (light client) | Ready | Ed25519 signature verification against validator set |
| Signature verification (snapshot) | Ready | Ed25519 verification with fallback |
| MEV protection | Future | Commit-reveal library exists, not integrated |

---

## Test Status

- `cargo test -p call-protocol` (security tests) — covers block limits, mempool rate limiting, address saturation, replay protection, shielded per-block limits, nullifier double-spend, P2P rate limiting, consensus double-sign detection
- `cargo test -p call-network` (limits tests) — covers default limits, custom limits, validation bounds
- `cargo test -p call-consensus` — covers slashing economics, double-sign removal, offline proportional slash
- `cargo test -p call-agent` — covers permission defaults, grant deduction, registration fee, revocation
- `cargo test -p call-rpc` — covers mempool defense integration in payment and EVM tx submission paths, EIP-191 signature verification for asset registration
- `cargo test -p call-protocol --test test_governance_flow` — covers governance executor authorization (proposer/validator-only execution)

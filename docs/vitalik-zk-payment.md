# Vitalik ZK Payments Vision vs. Callchain Shielded Pool

**Date**: 2026-05-11
**Context**: Ethereum co-founder Vitalik Buterin published a research post on May 10, 2026, identifying Zero-Knowledge (ZK) Payments as the essential next standard for the global digital economy. This document compares that vision against Callchain's existing `ShieldedPrecompile` (`0x202`) implementation.

---

## 1. Vitalik's Vision: Core Tenets

In his May 10, 2026 post, Vitalik argued that for crypto-payments to achieve mass adoption, they must move beyond "pseudonymity" toward **"default privacy."**

### Key Proposals

| Theme | Detail |
|-------|--------|
| **Default Privacy** | Standard transfers should be replaced by ZK-proof-based transactions by default. |
| **Recursive SNARKs** | Use recursive SNARKs to process private payments at the same speed and cost as transparent ones. |
| **AI Agent Payments** | Autonomous AI agents need a way to pay for services (e.g., LLM API credits) without leaving a traceable breadcrumb trail back to their human owners. |
| **ZK API Usage Credits** | A specific abstraction for private, metered service payments. |
| **Selective Disclosure** | Users can provide specific "compliance proofs" to authorized entities or tax authorities without leaking data to the general public. |
| **Proof of Innocence** | A mechanism to prove funds are legitimate without revealing full transaction history. |
| **Compete with Visa** | Privacy must be "invisible" and seamless to rival legacy payment processors. |

---

## 2. Callchain Shielded Pool: Current Implementation

Callchain's privacy layer is implemented via the `ShieldedPrecompile` at address `0x202` (`crates/shielded/`). It provides a **UTXO-based shielded pool** with real Groth16 zk-SNARK verification.

### Architecture Overview

```
Transparent Balance          Shielded Pool (0x202)          Transparent Balance
     |                             |                                |
     |-- deposit() --------------->|                                |
     |   (public amount)           |-- transfer() ----------------->|
     |                             |   (ZK proof verified)          |
     |                             |                                |
     |<-- withdraw() --------------|                                |
     |   (public amount)           |                                |
```

### Technical Stack

| Component | Implementation |
|-----------|---------------|
| **Proof System** | Groth16 on BN254 (arkworks 0.4) |
| **Circuits** | 3 independent circuits: `DepositCircuit` (~350 constraints), `WithdrawCircuit` (~3,551 constraints), `TransferCircuit` (~7,800 constraints) |
| **Hash Function** | Poseidon (plain + R1CS gadget) |
| **Note Model** | UTXO-based: `value`, `asset_id`, `rcm`, `recipient_ivk`, `rho` |
| **Note Encryption** | ChaCha20-Poly1305 |
| **Merkle Tree** | Poseidon Merkle Tree, depth 32, append-only |
| **Double-Spend Prevention** | Nullifier set (HashSet + BitSet compression) |
| **Key Hierarchy** | `spending_key` -> `incoming_view_key` (IVK) -> `full_view_key` (FVK) |
| **Performance** | Proof generation: 0.5-3s; Verification: ~3ms; Proof size: ~128B |
| **Prover Options** | Local (CPU) or remote HTTP prover server (`call-prover`) |

### Precompile Interface (0x202)

| Function | Type | Description |
|----------|------|-------------|
| `deposit(uint64 assetId, uint128 amount, bytes32 commitment)` | write | Move transparent balance into shielded pool |
| `transfer(uint64 assetId, bytes proof, bytes32[] nullifiers, bytes32[] commitments)` | write | Shielded-to-shielded transfer (ZK proof required) |
| `withdraw(uint64 assetId, address target, uint128 amount, bytes32 nullifier, bytes32 merkleRoot, bytes proofData)` | write | Move shielded balance back to transparent |
| `getMerkleRoot()` | view | Current Merkle tree root |
| `isNullifierSpent(bytes32)` | view | Check if a note has been spent |

### Compliance Framework

Four `ShieldedComplianceMode` variants exist:

| Mode | Behavior |
|------|----------|
| `Unrestricted` | Full privacy, no compliance checks |
| `KycRequired` | Sender/receiver must be KYC-verified |
| `IssuerAuditable` | Asset issuer can audit via shared viewing key |
| `WhitelistedOnly` | Only whitelisted addresses may participate |

---

## 3. Side-by-Side Comparison

| Dimension | Vitalik Vision | Callchain Implementation | Assessment |
|-----------|---------------|-------------------------|------------|
| **Default Privacy** | Transparent transfers should be *replaced* by ZK transfers; privacy is the default | Privacy is **opt-in** via explicit `deposit`/`transfer`/`withdraw` calls; transparent `AssetPrecompile.transfer()` remains the default path | **Gap**: Privacy is opt-in, not default |
| **Recursive SNARKs** | Recursive SNARKs to compress/aggregate proofs so privacy costs the same as transparency | Independent Groth16 proofs per transaction; no recursive aggregation; total verification cost scales linearly with tx count | **Gap**: No proof aggregation or recursion |
| **AI Agent Payments** | "ZK API Usage Credits" -- agents pay anonymously without linking to human owners | `AgentPrecompile` (`0x209`) handles agent registration and balance grants, but agent payments flow through **transparent** protocol balances | **Gap**: Agent and Shielded systems are not integrated |
| **Selective Disclosure** | Users disclose *only* what is necessary to authorized parties | `ViewingKey` (IVK/FVK) allows full decryption of all related notes by key holder; no granularity | **Gap**: All-or-nothing disclosure; no selective proof |
| **Proof of Innocence** | Prove funds are legitimate without revealing history | Not implemented. Compliance modes use KYC/whitelist checks or full viewing-key audit | **Gap**: No zero-knowledge "proof of innocence" mechanism |
| **AML / Regulatory** | Compliance proofs for authorities without public exposure | `IssuerAuditable` mode lets issuers audit; `KycRequired` verifies against registry | **Partially Aligned**: Compliance exists but lacks ZK-based proof of innocence |
| **Performance Target** | Same speed/cost as transparent transactions | 250ms block time, ~3ms verification per proof, 50 shielded tx/block limit | **Aligned**: On-chain verification performance meets retail payment requirements |
| **Proof Generation** | Should be fast enough for real-time UX | Local: 0.5-3s; Remote prover server (`call-prover`): GPU-accelerated | **Aligned**: Server-side prover mitigates client-side latency |
| **User Experience** | Privacy is "invisible" -- users should not know crypto is involved | Requires explicit deposit into pool, managing viewing keys, understanding nullifiers/commitments | **Gap**: UX is power-user oriented |

---

## 4. Key Gaps & Recommended Directions

### Gap 1: Privacy is Opt-In, Not Default

**Current State**: Users must explicitly call `deposit()` to enter the shielded pool. Most transactions flow through transparent `AssetPrecompile` (`0x202`).

**Vitalik's Position**: "Default privacy" means the *standard* transfer path is shielded.

**Potential Directions**:
- **Wallet-layer default routing**: Wallets default to `switchToEvm` -> `deposit` -> `transfer` -> `withdraw` flow for all payments.
- **Protocol-layer auto-shielding**: New balances are automatically deposited into the shielded pool upon receipt.
- **Unified transfer API**: A single `pay()` abstraction that routes transparent or shielded based on recipient support.

### Gap 2: No Recursive SNARKs / Proof Aggregation

**Current State**: Each `transfer()` carries an independent Groth16 proof. Verifying 50 shielded tx in a block costs ~150ms (50 x 3ms).

**Vitalik's Position**: Recursive SNARKs allow many private payments to be verified at the cost of one.

**Potential Directions**:
- **Batch verification**: Verify multiple Groth16 proofs with shared randomness (reduces pairing overhead).
- **Halo2 migration**: The `Prover` trait already abstracts the backend; a Halo2 prover could provide universal SRS and recursive composition.
- **Rollup-style aggregation**: A separate aggregator node collects shielded tx, generates a single recursive proof, and submits to the chain.

### Gap 3: No Proof of Innocence

**Current State**: To prove compliance, a user must share their `full_view_key` with an auditor, who can then decrypt *all* related transactions.

**Vitalik's Position**: Users should prove innocence without revealing history.

**Potential Directions**:
- **ZK membership proof circuit**: Prove "my input notes descend from a set of whitelisted deposit addresses" without revealing which ones.
- **ZK range proof for source age**: Prove "all my funds have been in the shielded pool for > 90 days" (indicating non-tainted origin).
- **Selective note disclosure**: Prove properties about specific notes without revealing the full viewing key.

### Gap 4: Agent and Privacy Not Integrated

**Current State**: `AgentPrecompile` grants transparent protocol balances to agents. Agents paying for services leave a fully traceable trail.

**Vitalik's Position**: AI agents need anonymous payment rails.

**Potential Directions**:
- **Shielded agent sub-accounts**: Derive agent-specific spending keys from the owner's master key; owner retains FVK for audit.
- **ZK API Usage Credits precompile**: A new precompile or extension to `AgentPrecompile` that allows metered, anonymous service payments using shielded notes.
- **Agent proof delegation**: Agent holds a delegated proving key that can generate shielded transfer proofs on behalf of the owner without accessing the master spending key.

---

## 5. Where Callchain is Already Aligned

Despite the gaps above, Callchain's shielded implementation already satisfies several critical requirements of Vitalik's vision:

| Alignment | Evidence |
|-----------|----------|
| **Real ZK Proofs** | Full Groth16 circuits (Deposit/Withdraw/Transfer) with arkworks; 120+ passing tests; no mock stubs in production path |
| **EVM-Native Verification** | BN254 curve chosen specifically for EVM `ecPairing` precompile compatibility; on-chain verification at ~285k gas |
| **Viewing Key Hierarchy** | `spending_key` -> `IVK` -> `FVK` with domain-separated Poseidon derivation; allows audit without spend capability |
| **Compliance Framework** | 4 compliance modes already defined and tested; extensible to new regulatory requirements |
| **Production Key Infrastructure** | `production-keys` feature + Powers of Tau ceremony scripts + key rotation via governance |
| **Remote Prover Service** | `call-prover` HTTP server offloads proof generation from client devices |
| **Performance** | 3ms verification, 128B proof size, 50 tx/block -- suitable for retail payment throughput |

---

## 6. Summary

> **Callchain's Shielded Pool is "technology-ready, product-opt-in."**

The core cryptographic infrastructure -- Groth16 circuits, Poseidon hashing, nullifier sets, Merkle trees, viewing keys, and compliance modes -- is fully implemented and tested. The gap is not in engineering but in **protocol defaults and integrations**:

1. **Privacy is a feature, not the default** -- users must explicitly opt into the shielded pool.
2. **No proof aggregation** -- each tx is independently verified; recursive SNARKs would reduce marginal cost to near-zero.
3. **No proof of innocence** -- compliance relies on full viewing-key disclosure rather than zero-knowledge proofs.
4. **Agents use transparent balances** -- the `AgentPrecompile` and `ShieldedPrecompile` operate in silos.

To fully align with Vitalik's ZK Payments vision, Callchain would need protocol-level decisions to make shielded the default path, plus additional circuits for recursive aggregation and selective compliance proofs.

---

## References

- Vitalik Buterin research post, May 10, 2026 ( summarized above )
- `docs/zk.md` -- Callchain ZK Shielded Transaction Design (v0.2.0)
- `crates/shielded/src/precompile.rs` -- `ShieldedPrecompile` (`0x202`)
- `crates/shielded/src/lib.rs` -- `ShieldedStorage`, `ZkProof`, `ViewingKey`
- `crates/shielded/src/circuit_*.rs` -- R1CS circuit implementations
- `crates/agent/src/precompile.rs` -- `AgentPrecompile` (`0x209`)

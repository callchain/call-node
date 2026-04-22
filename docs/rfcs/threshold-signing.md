# RFC: Threshold Signing for Callchain Validators

**Status**: Draft
**Author**: Callchain Protocol Team
**Date**: 2026-04-22
**Target**: Mainnet v1.0

---

## 1. Summary

This RFC proposes a threshold Ed25519 signing scheme for Callchain validators, enabling M-of-N distributed key generation and signature aggregation. This eliminates single points of failure in validator key management and aligns with production security requirements.

---

## 2. Motivation

### Current State

The `Signer` trait supports three backends (local, AWS KMS, HashiCorp Vault) but all are **single-party**: one private key produces one signature. This creates operational risk:

- **Key compromise**: A stolen validator key can produce unilateral slashes or consensus forks.
- **Availability**: If the single signer node fails, the validator cannot participate.
- **Custody**: No native support for multi-party custody (e.g., 3-of-5 operators).

### Goals

1. **M-of-N signing**: Any `M` of `N` key shares can produce a valid Ed25519 signature.
2. **No trusted dealer**: Use a DKG protocol so no single party ever holds the full private key.
3. **Compatible with existing consensus**: The aggregated signature is a standard Ed25519 signature — no changes to `Block` verification logic.
4. **Rotatable shares**: Support share refresh without changing the public key.

---

## 3. Design

### 3.1 Cryptographic Primitive

We use **FROST (Flexible Round-Optimised Schnorr Threshold)** over Ed25519, specifically:

- **FROST-Ed25519** as specified in [draft-irtf-cfrg-frost-14](https://datatracker.ietf.org/doc/draft-irtf-cfrg-frost/)
- Reference implementation: `frost-ed25519` crate (based on `curve25519-dalek`)

Why FROST over other schemes:

| Scheme | Rounds | Identifiable Aborts | Ed25519 Native | Notes |
|--------|--------|---------------------|----------------|-------|
| FROST | 1 (signing) | Yes | Yes | Industry standard, audited |
| SHNR | 2 | No | No | Requires adapter |
| CMP | 6+ | Yes | No | Complex, overkill |

### 3.2 Architecture

```
Validator Node (N operator groups)
|
|-- ThresholdSigner (share_id, key_share)
|   |
|   |-- DKG Coordinator (bootstrap only)
|   |   |-- Round 1: Commitment exchange
|   |   |-- Round 2: Key share derivation
|   |
|   |-- Signing Session
|   |   |-- Round 1: Nonce commitment (broadcast)
|   |   |-- Round 2: Signature share (unicast to aggregator)
|   |
|   |-- Signature Aggregator (one participant)
|       |-- Validate signature shares
|       |-- Combine into final Ed25519 signature
|
|-- Consensus engine receives standard Signature
```

### 3.3 Key Types

```rust
/// A single key share held by one operator.
pub struct KeyShare {
    /// Participant identifier (1..=N)
    pub id: u32,
    /// Scalar share of the private key
    pub scalar: [u8; 32],
    /// Public key of the group (same for all shares)
    pub group_public: Ed25519PublicKey,
    /// Verifiable proof that this share is correct
    pub proof_of_knowledge: Vec<u8>,
}

/// Configuration for threshold parameters.
pub struct ThresholdConfig {
    /// Total number of shares (N)
    pub total_shares: u32,
    /// Minimum shares needed to sign (M)
    pub threshold: u32,
    /// Public keys of all participants (for DKG)
    pub participant_pubkeys: Vec<Ed25519PublicKey>,
}
```

### 3.4 DKG Protocol (Trusted Dealer Alternative)

For testnet and controlled deployments, a **trusted dealer** generates the key, splits it via Shamir, and distributes shares. For mainnet, we require **FROST DKG**:

1. **Round 1**: Each participant generates a random polynomial and broadcasts a commitment to its coefficients.
2. **Round 2**: Each participant sends encrypted secret shares to every other participant.
3. **Verification**: Each participant verifies received shares against the Round 1 commitments.
4. **Aggregation**: The public key is derived from the sum of all commitment coefficients.

```rust
pub trait DkgProtocol {
    /// Initiate DKG as participant `id`.
    fn initiate(&self, config: ThresholdConfig) -> DkgRound1;

    /// Process Round 1 commitments from all participants.
    fn round2(&self, round1: DkgRound1, commitments: Vec<DkgCommitment>) -> DkgRound2;

    /// Derive the final key share after receiving all Round 2 shares.
    fn finalize(&self, round2: DkgRound2, shares: Vec<EncryptedShare>) -> Result<KeyShare, DkgError>;
}
```

### 3.5 Signing Protocol

FROST signing requires exactly **one round of broadcast + one round of aggregation**:

```rust
pub trait ThresholdSigner {
    /// Initiate a signing session for a message.
    fn sign_round1(&self, msg: &[u8; 32]) -> SigningCommitment;

    /// Produce a signature share given others' commitments.
    fn sign_round2(
        &self,
        msg: &[u8; 32],
        commitments: Vec<SigningCommitment>,
    ) -> Result<SignatureShare, SigningError>;
}

/// Aggregator collects shares and produces the final signature.
pub struct SignatureAggregator;

impl SignatureAggregator {
    pub fn aggregate(
        msg: &[u8; 32],
        group_public: &Ed25519PublicKey,
        commitments: Vec<SigningCommitment>,
        shares: Vec<SignatureShare>,
    ) -> Result<Signature, AggregationError> {
        // FROST aggregation logic
    }
}
```

The final `Signature` is a standard Ed25519 signature — it validates with `ed25519_verify()` without any threshold awareness.

### 3.6 Integration with Callchain Consensus

```rust
pub struct ThresholdConsensusSigner {
    share: KeyShare,
    config: ThresholdConfig,
    /// Network transport for commitment/share exchange
    transport: ThresholdTransport,
}

impl call_crypto::Signer for ThresholdConsensusSigner {
    fn sign(&self, msg: &[u8; 32]) -> Result<Signature, SignerError> {
        // 1. Broadcast Round 1 commitment to all peers
        let commitment = self.sign_round1(msg);
        self.transport.broadcast_commitment(commitment)?;

        // 2. Collect M-1 commitments from peers
        let peer_commitments = self.transport.collect_commitments(
            self.config.threshold as usize - 1,
            TIMEOUT_MS,
        )?;

        // 3. Produce signature share
        let share = self.sign_round2(msg, peer_commitments)?;
        self.transport.send_share_to_aggregator(share)?;

        // 4. Aggregator (could be self or another peer) combines
        let signature = self.transport.await_aggregate_signature(TIMEOUT_MS)?;
        Ok(signature)
    }
}
```

---

## 4. Security Considerations

### 4.1 Threat Model

| Threat | Mitigation |
|--------|------------|
| Share compromise ( < M shares) | FROST security: fewer than M shares reveal nothing |
| Malicious aggregator | Signature shares are verifiable; aggregator cannot forge |
| Replay attack | Each signing session uses fresh random nonces |
| DKG rogue-key attack | FROST DKG includes proof-of-knowledge for each commitment |
| Network partition | Signing fails if < M participants reachable (liveness, not safety) |

### 4.2 Share Refresh

To rotate shares without changing the group public key:

1. Participants run a **proactive secret sharing** refresh round.
2. Each participant generates a random zero-sharing polynomial.
3. New shares are derived from old share + zero-share.
4. Old shares are securely destroyed.

This should be performed:
- After any share is exposed (compromise recovery)
- On a scheduled basis (e.g., quarterly)
- Before major protocol upgrades

### 4.3 Backup and Recovery

- **Never** back up shares to cloud storage unencrypted.
- Use HSM-backed share storage where available.
- Each share should be encrypted with a distinct passphrase / hardware token.
- Maintain an offline `N-of-N` recovery ceremony document.

---

## 5. Implementation Plan

### Phase 1: FROST Library Integration (v0.2.0)

- [ ] Add `frost-ed25519` dependency (or vendor if licensing conflicts)
- [ ] Implement `ThresholdSigner` trait wrapper
- [ ] Unit tests: DKG, signing, aggregation, corruption detection
- [ ] Benchmark: signing latency with M=7, N=21

### Phase 2: Network Transport (v0.3.0)

- [ ] P2P commitment/share exchange over libp2p or dedicated channels
- [ ] Timeout and retry logic
- [ ] Identifiable abort: detect and report which participant misbehaved

### Phase 3: Consensus Integration (v0.4.0)

- [ ] Implement `Signer` trait for `ThresholdConsensusSigner`
- [ ] Configurable via `config.toml`: threshold parameters, peer addresses
- [ ] Integration test: 5-node threshold validator set on devnet

### Phase 4: Production Hardening (v1.0)

- [ ] HSM-backed share storage (YubiHSM, AWS CloudHSM)
- [ ] Share refresh automation
- [ ] Audit by third-party cryptography firm
- [ ] Run bug bounty focused on threshold signing

---

## 6. Open Questions

1. **DKG vs Trusted Dealer for testnet?**
   Testnet may use trusted dealer for simplicity, but devnet should exercise DKG to catch issues early.

2. **Aggregator selection?**
   Options: round-robin, lowest participant ID, or VRF-based. Round-robin is simplest.

3. **Network layer?**
   Reuse existing libp2p gossipsub or implement direct p2p channels? Direct channels reduce latency but add connection management.

4. **Identity binding?**
   How do we map on-chain validator IDs to threshold participants? Proposal: each validator registers a `ThresholdConfig` on-chain at stake time.

---

## 7. References

- [FROST: Flexible Round-Optimized Schnorr Threshold Signatures](https://datatracker.ietf.org/doc/draft-irtf-cfrg-frost/)
- [Shamir's Secret Sharing](https://en.wikipedia.org/wiki/Shamir%27s_Secret_Sharing)
- [Ed25519: High-speed high-security signatures](https://ed25519.cr.yp.to/)
- `commonware-cryptography` threshold signing design (reference only)

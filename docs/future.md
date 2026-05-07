# Future Work

Items deferred from the 2026-05-06 system review. These are not blockers for
devnet/testnet but should be addressed before mainnet readiness.

---

## EthLightClient BLS Consensus Verification

**Context:** `crates/light-client` Ethereum light client currently verifies headers
via parent-hash chain only. It does not verify Ethereum consensus layer BLS
aggregate signatures from the beacon chain sync committee.

**Why deferred:** The parent-hash chain + `set_finalized_block()` checkpoint
tracking is sufficient for bridge deposit validation on devnet and testnet.
Full consensus verification requires integrating Ethereum beacon chain light
client sync (Altair sync committees, ~512 validators per period), which is a
significant scope increase.

**When to revisit:** Before mainnet bridge launch. At that point the bridge
must trustlessly verify Ethereum finality without relying on an externally-set
checkpoint.

---

## Prover Key Rotation

**Context:** `call-shielded::prover::RealProver` uses `OnceLock` global static
for proving/verification keys. There is no runtime mechanism to rotate keys
without restarting the prover service.

**Why deferred:** Key rotation requires a governance-driven ceremony
(coordinated trusted setup, new CRS distribution, verifying-key hash update in
code). This is a mainnet-readiness procedure, not a devnet/testnet concern.

**When to revisit:** Before mainnet shielded pool launch. Plan:
1. Governance proposal type for `ProverKeyRotation`
2. Ceremony coordination (offline MPC)
3. Service hot-reload of new verification key
4. Old key sunset period for in-flight proofs

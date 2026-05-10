# BLS Consensus Verification for Ethereum Light Client

## Overview

The Callchain light client supports **Ethereum Altair light client sync** to verify
that execution-layer headers are backed by the beacon chain consensus. This
eliminates the need to fully trust the `parent_hash` chain and instead
confirms headers with **BLS12-381 aggregate signatures** from the Ethereum
sync committee (512 validators).

## Architecture

```text
┌─────────────────────┐      ┌─────────────────────┐
│  Beacon REST API    │      │  Ethereum JSON-RPC  │
│  (SSZ)              │      │  (RLP headers)      │
└─────────┬───────────┘      └─────────┬───────────┘
          │                            │
          ▼                            ▼
┌──────────────────────────────────────────────┐
│         LightClientUpdate (SSZ)              │
│  • attested_header                           │
│  • finalized_header                          │
│  • sync_aggregate (BLS sig + bitmask)        │
│  • next_sync_committee                       │
└─────────┬────────────────────────────────────┘
          │
          ▼
┌──────────────────────────────────────────────┐
│  EthLightClient::apply_light_client_update   │
│                                              │
│  1. Compute sync committee signing root      │
│     hash_tree_root(SigningData {             │
│       object_root: attested_header.root,     │
│       domain: domain_sync_committee          │
│     })                                       │
│                                              │
│  2. Extract participant pubkeys from bits    │
│     sync_committee.participant_pubkeys(bits) │
│                                              │
│  3. Verify BLS aggregate signature           │
│     bls_verify_aggregate_beacon(             │
│       pubkeys, signing_root, signature       │
│     )                                        │
│                                              │
│  4. Check participation > 2/3 (342/512)      │
│                                              │
│  5. Update sync committee if period advanced │
│                                              │
│  6. Return finalized_header (slot, root)     │
└─────────┬────────────────────────────────────┘
          │
          ▼
┌──────────────────────────────────────────────┐
│  set_finalized_block(execution_block, hash)  │
│  → Headers ≤ finalized are consensus-safe    │
└──────────────────────────────────────────────┘
```

## Key Types

### `BeaconConfig`

Chain-level constants needed to compute the BLS signing domain:

```rust
pub struct BeaconConfig {
    /// Current fork version (e.g. `[0, 0, 0, 1]` for Altair)
    pub fork_version: [u8; 4],
    /// Genesis validators root (32 bytes), fixed at chain genesis
    pub genesis_validators_root: B256,
}
```

### `LightClientUpdate`

An Altair light client update containing everything needed to advance the
light client to a newer sync period:

```rust
pub struct LightClientUpdate {
    pub attested_header: BeaconBlockHeader,        // 112 bytes SSZ
    pub next_sync_committee: SyncCommittee,        // 24624 bytes
    pub next_sync_committee_branch: [B256; 5],     // merkle proof
    pub finalized_header: BeaconBlockHeader,       // 112 bytes
    pub finality_branch: [B256; 6],                // merkle proof
    pub sync_aggregate: SyncAggregate,             // 160 bytes
    pub signature_slot: u64,                       // 8 bytes
}
```

Total: **25368 bytes** (all fixed-size, manually decoded from SSZ).

### `SyncCommittee`

A committee of 512 BLS public keys plus an aggregate pubkey:

```rust
pub struct SyncCommittee {
    pub pubkeys: Vec<BlsPublicKey>,   // 512 × 48 bytes
    pub aggregate_pubkey: BlsPublicKey, // 48 bytes
}
```

### `SyncAggregate`

The aggregate signature + participation bitmask:

```rust
pub struct SyncAggregate {
    pub sync_committee_bits: [u8; 64],    // bit i = validator i signed
    pub sync_committee_signature: BlsSignature, // 96 bytes
}
```

## BLS Signature Verification

### Signing Root Computation

The sync committee signs a beacon block header. The message is:

```
signing_root = hash_tree_root(SigningData {
    object_root: attested_header.hash_tree_root(),
    domain: compute_domain_sync_committee(fork_version, genesis_validators_root),
})
```

Where the domain is:

```
domain[0..4]   = DOMAIN_SYNC_COMMITTEE = [0x07, 0x00, 0x00, 0x00]
domain[4..32]  = first 28 bytes of keccak256(fork_version || genesis_validators_root)
```

### DST (Domain Separation Tag)

Ethereum beacon chain uses the **proof-of-possession** DST:

```
BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_
```

This differs from Callchain's native NUL DST and is handled by the
`bls_verify_aggregate_beacon()` function in `call_crypto`.

## Usage

### Initialize with Beacon Config

```rust
use call_light_client::{EthLightClient, GenesisState, BeaconConfig};

let genesis = GenesisState {
    anchor_hash: B256::from(...),
    anchor_block: 21000000,
    state_root: B256::from(...),
};

let beacon_config = BeaconConfig {
    fork_version: [0, 0, 0, 1],  // Altair
    genesis_validators_root: B256::from(...),
};

let mut client = EthLightClient::init_with_beacon_config(
    genesis,
    Some(beacon_config),
);
```

### Apply Light Client Update

```rust
use call_light_client::sync::fetch_light_client_finality_update;

let update = fetch_light_client_finality_update("https://beacon-api.example.com")?;
let (finalized_slot, finalized_root) = client.apply_light_client_update(update)?;

// Map beacon slot to execution block number (approx: slot × 12s)
// Then mark execution headers as finalized
client.set_finalized_block(execution_block_number, execution_block_hash);
```

### Check Consensus Verification

```rust
if client.is_consensus_verified(block_number) {
    // This header is backed by beacon chain consensus
    // Safe for bridge deposits
}
```

## Sync Periods

The sync committee rotates every **8192 slots** (~27 hours):

```
sync_period = slot // 8192
```

When `apply_light_client_update()` receives an update whose signature slot
falls in a newer period, the `next_sync_committee` from the update becomes
the current committee for future verifications.

## Security Properties

| Property | Mechanism |
|----------|-----------|
| **Canonical chain** | `parent_hash` chain from trusted anchor |
| **Consensus finality** | BLS aggregate signature from 512-member sync committee |
| **Supermajority** | > 2/3 participation required (342/512 validators) |
| **No reorg below finalized** | `handle_reorg()` rejects fork points ≤ finalized block |
| **Period rotation** | Sync committee updates authenticated by previous committee |

## Comparison: With vs Without Beacon Verification

| Feature | Parent-hash only | + Beacon BLS |
|---------|-----------------|--------------|
| Trust model | Trust anchor + RPC source | Trust anchor + consensus majority |
| Reorg resistance | Manual buffer (64 headers) | Finalized checkpoints |
| Deposit safety | Best-effort | Cryptographically proven |
| Network calls | `eth_getBlockByNumber` | + beacon light_client endpoints |
| Latency | ~12s (1 block) | ~12 min (64 slots = 1 epoch) |

## Error Handling

| Error | Cause |
|-------|-------|
| `NotInitialized` | `init_with_beacon_config()` not called with `Some(config)` |
| `SyncCommitteeSignatureInvalid` | BLS aggregate verification failed |
| `InsufficientSyncParticipation` | Fewer than 342 validators signed |
| `BeforeFinalized` | Reorg attempt below finalized checkpoint |

## References

- [Ethereum Altair Light Client Spec](https://github.com/ethereum/consensus-specs/blob/dev/specs/altair/light-client/sync-protocol.md)
- [SSZ Serialization](https://github.com/ethereum/consensus-specs/blob/dev/ssz/simple-serialize.md)
- [BLS Signatures (EIP-2333)](https://eips.ethereum.org/EIPS/eip-2333)

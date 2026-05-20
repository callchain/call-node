# Validator Key Rotation

> **Document**: ValidatorKeyRotation mechanism and on-chain key switching.
>
> Last updated: 2026-05-11

## Table of Contents

- [1. Overview](#1-overview)
- [2. Key Types](#2-key-types)
- [3. Rotation Flow](#3-rotation-flow)
- [4. On-Chain Switch Mechanism](#4-on-chain-switch-mechanism)
- [5. Execution Data Format](#5-execution-data-format)
- [6. Security Considerations](#6-security-considerations)
- [7. Operational Guide](#7-operational-guide)
- [8. Code References](#8-code-references)

---

## 1. Overview

`ValidatorKeyRotation` (proposal type 9) allows an on-chain governance proposal to change a validator's **ed25519 consensus public key** without unstaking and restaking. This is critical for key compromise recovery and routine key rotation.

The rotation is **governed**: it must pass voting, timelock, and execution before taking effect.

**Caveat**: Only the ed25519 consensus key is rotated. The BLS12-381 aggregated vote key is generated at node startup and is **not** covered by this mechanism.

---

## 2. Key Types

| Key | Purpose | Rotation Mechanism |
|---|---|---|
| **ed25519** | Validator identity, block signing, governance voting | `ValidatorKeyRotation` proposal (type 9) |
| **secp256k1** | EVM transaction signing | Independent; derived from validator's EVM address |
| **BLS12-381** | Aggregated vote signatures | Generated at boot; no on-chain rotation |

---

## 3. Rotation Flow

```
┌─────────────────────────────────────────────────────────────┐
│  1. Proposer submits ValidatorKeyRotation proposal          │
│     (type=9, execution_data encodes validatorId + newPk)    │
│                                                             │
│  2. Voting period (validators vote with secp256k1 sigs)    │
│                                                             │
│  3. Quorum reached → proposal enters Queued state           │
│                                                             │
│  4. Timelock expires → GovernanceAdvancer executes          │
│                                                             │
│  5. On-chain pubkey overwritten in EVM storage              │
│                                                             │
│  6. Validator restarts node with new private key            │
└─────────────────────────────────────────────────────────────┘
```

The state machine is handled by `GovernanceAdvancer` (see `crates/node/src/governance_advancer.rs`).

---

## 4. On-Chain Switch Mechanism

When the proposal is executed, the advancer performs an **atomic overwrite** of the validator's public key in EVM storage:

```rust
// governance_advancer.rs
let validator_id = u64::from_be_bytes(id_buf);
let validator_addr = sa::read_validator_addr(evm_state, validator_id);
if validator_addr != Address::ZERO {
    sa::rotate_validator_key_evm(evm_state, validator_addr, pubkey_buf);
}
```

`rotate_validator_key_evm` writes to the same storage slot used during initial staking:

```rust
// state_accessors.rs
pub fn rotate_validator_key_evm(
    evm_state: &mut dyn ProtocolStorage,
    addr: Address,
    new_ed25519_pubkey: [u8; 32],
) {
    evm_state.set_storage(
        VALIDATOR_ADDRESS,
        slot_validator_pubkey(addr),   // keccak256(addr || "pubkey")
        U256::from_be_slice(&new_ed25519_pubkey),
    );
}
```

### Characteristics

- **Instant**: There is no warm-up period or dual-key phase. The old key is overwritten immediately in the block where the proposal executes.
- **No history**: The old public key is not retained on-chain. There is no "previous key" record in EVM storage.
- **Atomic**: The update happens within a single block's state transition. Either the entire block commits (including the key change) or it reverts.

---

## 5. Execution Data Format

The `execution_data` field of the governance proposal must be ABI-encoded as follows:

| Byte range | Type | Description |
|---|---|---|
| `0..24` | padding | Zero-padded to align with ABI dynamic types |
| `24..32` | `uint64` | `validator_id` (big-endian) |
| `32..64` | `bytes32` | Old ed25519 public key (32 bytes) |
| `64..96` | `bytes32` | New ed25519 public key (32 bytes) |

> **Note**: The current implementation uses a 96-byte `execution_data` buffer and reads `validator_id` at offset 24, the old pubkey at offset 32, and the new pubkey at offset 64.

---

## 6. Security Considerations

### 6.1 No Transition Period

Because the switch is a single storage write, validators must be prepared to sign with the new key **before** the proposal executes. If a validator rotates to a key whose private key is not yet loaded, they will produce invalid signatures and risk being marked offline.

**Recommended practice**:
1. Generate new keypair offline.
2. Submit rotation proposal.
3. Wait for proposal to pass timelock.
4. **Before execution block**: update node config / HSM to use new key.
5. Proposal executes; node already has the new key.

### 6.2 BLS Key Not Rotated

The BLS12-381 key used for aggregated voting is generated randomly at node boot (`boot.rs:175`) and registered in EVM state. `ValidatorKeyRotation` does **not** update this key. If BLS key compromise is suspected, the validator must unstake and restake.

### 6.3 Governance Risk

A malicious majority could rotate an honest validator's key to an attacker-controlled one, effectively ejecting the validator from consensus without unstaking. This is why the governance threshold and timelock must be set conservatively.

### 6.4 Audit Trail

In addition to the pubkey update, the advancer writes an audit flag to governance storage:

```rust
evm_state.set_storage(
    GOVERNANCE_ADDRESS,
    storage_slot(&[b"key_rotation", &id_buf]),
    U256::from(1u8),
);
```

This allows external indexers to detect that a rotation occurred for a given proposal ID, even though the old key itself is not stored.

---

## 7. Operational Guide

### Submit a Key Rotation Proposal

Use the governance precompile (address `0x203`) or the RPC method `call_governanceSubmitProposal`.

Example execution data construction (Rust pseudo-code):

```rust
let validator_id: u64 = 7;
let old_pubkey: [u8; 32] = /* current ed25519 pubkey */;
let new_pubkey: [u8; 32] = /* new ed25519 pubkey */;

let mut exec_data = vec![0u8; 96];
exec_data[24..32].copy_from_slice(&validator_id.to_be_bytes());
exec_data[32..64].copy_from_slice(&old_pubkey);
exec_data[64..96].copy_from_slice(&new_pubkey);
```

### Node Operator Checklist

- [ ] New private key is backed up securely (HSM / keyring / Vault).
- [ ] New public key has been cross-checked off-chain.
- [ ] Rotation proposal has passed voting and timelock.
- [ ] Node config / signer has been updated to new key **before** execution block.
- [ ] Old key material has been securely destroyed (if compromised).

---

## 8. Code References

| File | Purpose |
|---|---|
| `crates/node/src/governance_advancer.rs` | Proposal type 9 execution logic |
| `crates/consensus/src/exec/state_accessors.rs` | `rotate_validator_key_evm`, `read_validator_pubkey` |
| `crates/governance/src/proposal.rs` | Proposal type enum |
| `crates/crypto/src/signer.rs` | Local / HSM / keyring signer abstractions |
| `crates/node/src/boot.rs` | BLS key generation at startup |

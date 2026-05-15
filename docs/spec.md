# Callchain Specification

## Version Information

| Item | Value |
|------|------|
| Version | 0.1.0-draft |
| Date | 2026-04-13 |
| Status | Draft |
| Language | Rust |

---

## 1. Overview

Callchain is a high-performance Layer-1 blockchain featuring a **unified EVM execution domain**: all transactions execute within the EVM, with protocol-level operations (payments, asset management, compliance) accessed via native precompile addresses (`0x101`–`0x209`). Seamless asset flow between protocol-managed state and EVM contract state is achieved through a unified internal bridge mechanism.

### 1.1 Design Principles

- **Assets as First-Class Citizens**: Assets such as stablecoins have native balance mappings at the protocol layer, enjoying deterministic execution and fixed fees
- **Open Issuance**: Anyone can register assets on-chain without permission
- **EVM Compatibility**: The smart contract layer is fully Ethereum-compatible, allowing existing DeFi ecosystems to migrate seamlessly
- **Precompile-Based Protocol Access**: Protocol features (transfers, asset registry, compliance, shielded pool, agents) are invoked via EVM precompiles at fixed addresses (`0x101`–`0x209`), giving smart contracts direct access to native protocol state
- **Compliance Framework**: Protocol-level compliance policy engine, with issuers choosing their own strategies

### 1.2 Core Architecture

```
                    Callchain L1
              Simplex BFT Consensus (Single Validator Set)
                         │
                         ▼
                    EVM Contract
                 (Unified Execution Layer)
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
   Protocol State    ERC-20 Storage   Precompiles
   (Native Balance   (Contract        (0x101-0x209)
    Mapping)          Independent
                      Balances)
          │              │
          └──────────┬───┘
                     ▼
            Internal Bridge
           (Switch Precompile 0x207)
           Escrow or Mint/Burn per Asset
```

---

## 2. Consensus Layer

### 2.1 Algorithm

Uses the **Commonware Simplex BFT** consensus algorithm.

Simplex is a low-latency Byzantine Fault Tolerance consensus protocol, with a production-grade Rust implementation provided by Commonware (`commonware-consensus` crate). Its core design is a simplified BFT state machine where each round has a single proposer packing blocks, validators voting, and finality achieved at 2/3 majority.

**Core Features:**

- BFT Fault Tolerance: Tolerates 1/3 Byzantine validators
- Communication Complexity: O(n) -- only linear number of message exchanges per round
- Finality: Single-round confirmation, sub-second finality (~500ms, 2 rounds)
- Block Time: 250ms
- Graceful Degradation: Automatically pauses block production during network partitions, resumes immediately when partition recovers

**Rationale for Choosing Simplex:**

| Dimension | Simplex | Tendermint Family (Malachite) | HotStuff Family |
|------|---------|---------------------------|-------------|
| Communication Complexity | O(n) | O(n²) | O(n) |
| State Machine Complexity | Lowest | Medium | High |
| Rust Implementation Maturity | commonware-consensus ready to use | malachite-bft available | No mature open-source Rust implementation |
| Subset Rotation | Natively supported | Requires additional implementation | Natively supported |
| Audit Surface | Smallest | Medium | Large |
| Production Validation | Being validated on Tempo chain | Being validated on Arc chain | PlasmaBFT (closed source) |

**Key Design Decision:** With 216 validators, Simplex's O(n) = 216 messages/round, while Tendermint's O(n²) ≈ 46K messages/round. The 200x difference in communication volume directly determines the latency ceiling.

### 2.2 Dependencies

```toml
[dependencies]
commonware-consensus = "2026.3.0"
commonware-cryptography = "2026.3.0"
commonware-p2p = "2026.3.0"
commonware-runtime = "2026.3.0"
commonware-codec = "2026.3.0"

# Reth full integration (EVM + node framework + RPC + storage)
# Pinned to commit compatible with Alloy 1.8.2 to avoid crates.io version inconsistency
reth-chainspec = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-consensus = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-consensus-common = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-db = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-db-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-e2e-test-utils = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-engine-local = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-engine-tree = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-errors = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-consensus = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-engine-primitives = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-primitives = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-evm = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-evm-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-core = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-metrics = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-payload-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-provider = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-revm = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-eth-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-eth-types = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-storage-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-tracing = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-transaction-pool = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie-common = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie-db = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
```

### 2.3 Validators

| Parameter | Value |
|------|------|
| Validator Count | 100-216 |
| Per-Round Subset Size | 21 |
| Rotation Mechanism | Random selection per round |
| Minimum Stake | Dynamically adjusted (to keep validator count stable) |
| Block Reward | 50% fee distribution + 0% inflation |
| Slashing | Double-sign slashing + offline penalty |

### 2.4 Block Structure

```rust
struct Block {
    header: BlockHeader,
    evm_txs: Vec<EvmTx>,                     // EVM transactions (raw RLP bytes; may call precompiles)
}

struct BlockHeader {
    parent_hash: Hash,
    height: u64,
    timestamp_millis: u64,          // sub-second timestamp
    state_root: Hash,               // EVM state root (single source of truth)
    proposer: ValidatorId,
    signature: Signature,
    version: ProtocolVersion,       // protocol version (per spec §19)
    bls_aggregate_signature: Option<Vec<u8>>,  // BLS12-381 aggregated signature
    bls_signer_bitmap: Vec<u8>,     // bitmask of BLS signers
}
```

### 2.5 Block Execution Order

Each block is processed in the following order:

```
1. Execute EVM transactions (evm_txs)
   - Standard EVM calls, including precompile invocations at 0x101–0x209
   - Validator rewards, fee settlement, and bridge operations are all applied
     through EVM transactions that call the appropriate precompiles
2. Compute final state root, pack block header
```

---

## 3. Protocol Payment Layer

### 3.1 Asset Registry

Anyone can register assets. Upon registration, protocol-level balance mapping is automatically created.

```rust
struct Asset {
    id: AssetId,                    // unique ID assigned by protocol (u64)
    name: String,                   // display name, e.g. "USDC"
    symbol: String,                 // trading symbol, e.g. "USDC"
    decimals: u8,
    issuer: Address,                // issuer address
    total_supply: u128,             // protocol-layer total supply
    policy: CompliancePolicy,       // compliance policy
    registered_at: u64,             // registration timestamp
    evm_contract: Address,          // bound ERC-20 contract address (zero if none)
    has_erc20: u8,                  // 0 = protocol-only, 1 = ERC-20 bound
    dominance: u8,                  // 0 = EVM-dominant (escrow), 1 = PROTOCOL-dominant (mint/burn)
    status: AssetStatus,
}

enum AssetStatus {
    Active,                         // normal
    Frozen,                         // paused (triggerable by issuer)
    Delisted,                       // delisted
}
```

### 3.2 Registration Process

```rust
fn register_asset(
    caller: Address,
    name: String,
    symbol: String,
    decimals: u8,
    initial_supply: u128,
    policy: CompliancePolicy,
    registration_fee: Balance,      // economic anti-spam fee
) -> Result<AssetId>;
```

Upon registration:
1. Deduct registration fee (to prevent spam registration)
2. Assign unique AssetId
3. Create protocol-layer balance mapping, `balances[issuer] = initial_supply`
4. Asset starts as **protocol-only** (`has_erc20 = 0`, `dominance` unset)

**Binding to EVM (optional, issuer-only):**

After registration, the issuer can bind the asset to the EVM layer through one of two paths:

- **`registerErc20(contract)`** (`0x201`): Bind an existing external ERC-20 contract. Sets `has_erc20 = 1`, `dominance = 0` (EVM-dominant, escrow model). The Switch precompile (`0x207`) must hold tokens in escrow for deposits.
- **`createWrapper(assetId)`** (`0x201`): Deploy a system `WrappedToken` contract. Sets `has_erc20 = 1`, `dominance = 1` (PROTOCOL-dominant, mint/burn model). The Switch precompile can mint/burn tokens on demand.

`dominance` is immutable once set. Both paths require `caller == asset.issuer` and `has_erc20 == 0`.

### 3.3 Balance Management

```rust
/// Protocol-layer balance mapping
/// asset_id → (address → balance)
type ProtocolBalances = HashMap<AssetId, HashMap<Address, u128>>;

/// Allowance mapping (for approve/transferFrom)
/// (asset_id, owner, spender) → amount
type Allowances = HashMap<(AssetId, Address, Address), u128>;
```

### 3.4 Compliance Policy

```rust
enum CompliancePolicy {
    /// No restrictions, anyone can send/receive
    None,
    /// OFAC sanctioned address blacklist
    OfacBlacklist { blacklist_hash: Hash },
    /// Requires KYC proof (off-chain verification, on-chain marking)
    KycRequired,
    /// Whitelist addresses only
    Whitelist { registry: Address },
    /// Custom logic (via precompile callback)
    Custom { handler: Address },
}
```

The protocol layer enforces compliance checks before every transfer:

```rust
fn check_compliance(
    asset: &Asset,
    from: Address,
    to: Address,
) -> Result<()> {
    match asset.policy {
        CompliancePolicy::None => Ok(()),
        CompliancePolicy::OfacBlacklist { .. } => {
            ensure!(!is_sanctioned(from), "Sender is sanctioned");
            ensure!(!is_sanctioned(to), "Receiver is sanctioned");
            Ok(())
        }
        CompliancePolicy::KycRequired => {
            ensure!(is_kyc_verified(to), "Receiver not KYC verified");
            Ok(())
        }
        CompliancePolicy::Whitelist { registry } => {
            ensure!(is_whitelisted(registry, to), "Receiver not whitelisted");
            Ok(())
        }
        CompliancePolicy::Custom { handler } => {
            precompile_call(handler, from, to)
        }
    }
}
```

### 3.5 Protocol Precompile Invocation Model

All protocol operations are invoked via standard EVM transactions calling precompile addresses in the range `0x101`–`0x209`. The EVM execution engine (revm) routes these calls to native Rust handlers that operate on protocol state. Each precompile call is a separate EVM call frame; composability is achieved by wrapping multiple calls in a smart contract or by submitting multiple EVM transactions atomically within the same block.

**Precompile Address Ranges:**

| Address | Function |
|---------|----------|
| `0x101` | Oracle (price feeds, TWAP) |
| `0x103` | External bridge (deposit/withdraw/challenge) |
| `0x201` | Asset (transfer, batchTransfer, approve, transferFrom, mint, burn, register, registerErc20, createWrapper) |
| `0x202` | Shielded pool (deposit, transfer, withdraw) |
| `0x203` | Governance (propose, vote, queue, execute, emergency pause/resume) |
| `0x204` | Validator staking (stake, unstake, claimUnbonded) |
| `0x205` | Compliance (updateCompliance, checkCompliance) |
| `0x207` | Switch (internal bridge: switchToEvm / switchToProtocol) |
| `0x209` | Agent (register, grant, revoke) |
| `0x10a`–`0x1ff` | Reserved for future protocol operations |

**EVM Transaction Calling a Precompile:**

```rust
/// Standard EVM transaction (EIP-1559 or legacy) targeting a protocol precompile
struct EvmTx {
    to: Address,           // precompile address, e.g. 0x201 for transfer
    data: Vec<u8>,         // ABI-encoded precompile input
    value: u128,           // CALL value (usually 0 for precompile calls)
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    nonce: u64,
    chain_id: u64,
    signature: Signature,  // standard secp256k1 signature
}
```

**Example: Protocol transfer via precompile**

```solidity
// Solidity interface for the transfer precompile at 0x201
interface IProtocolTransfer {
    function transfer(
        uint64 assetId,
        address to,
        uint128 amount,
        bytes calldata memo
    ) external returns (bool);
}

// Usage from a contract or wallet
contract Example {
    IProtocolTransfer constant TRANSFER =
        IProtocolTransfer(0x0000000000000000000000000000000000000201);

    function pay(uint64 assetId, address recipient, uint128 amount) external {
        TRANSFER.transfer(assetId, recipient, amount, "");
    }
}
```

**Raw transaction example (Rust):**

```rust
let tx = EvmTx {
    to: Address::from_hex("0x201"),          // transfer precompile
    data: encode_abi("transfer", &[          // ABI-encoded arguments
        Token::Uint(asset_id.into()),
        Token::Address(recipient),
        Token::Uint(amount.into()),
        Token::Bytes(memo),
    ]),
    value: 0,
    gas_limit: 21_000,
    max_fee_per_gas: 10_000_000_000,
    max_priority_fee_per_gas: 1_000_000_000,
    nonce: 42,
    chain_id: 1,
    signature: sign_secp256k1(&private_key, &tx_hash),
};
```

**Precompile execution inside revm:**

```rust
fn call_precompile(address: Address, input: &[u8], gas_limit: u64) -> PrecompileResult {
    match address {
        0x101 => oracle_precompile(input, gas_limit),
        0x103 => bridge_precompile(input, gas_limit),
        0x201 => asset_precompile(input, gas_limit),
        0x202 => shielded_pool_precompile(input, gas_limit),
        0x203 => governance_precompile(input, gas_limit),
        0x204 => validator_precompile(input, gas_limit),
        0x205 => compliance_precompile(input, gas_limit),
        0x207 => switch_precompile(input, gas_limit),
        0x209 => agent_precompile(input, gas_limit),
        _ => Err(PrecompileError::NotFound),
    }
}
```

**Thread-Local State Sharing:**

Precompiles access EVM storage directly via `StorageRef`, a safe wrapper around revm's journal that decomposes the fat pointer without aliased mutable references. All protocol state (balances, assets, validators, etc.) is stored in EVM storage slots under precompile-specific addresses. Revm's existing Journal mechanism provides per-call atomicity and automatic rollback on failure.

### 3.6 Precompile Execution Semantics

Protocol operations execute inside revm as standard EVM precompile calls. Each precompile receives ABI-encoded input data and operates on EVM storage through `StorageRef`. Revm's existing Journal mechanism provides per-call atomicity and automatic rollback on failure; no separate protocol-state snapshot is needed.

```rust
/// Precompile execution entry point (called by revm during EVM execution)
fn execute_precompile(address: Address, input: &[u8], gas_limit: u64) -> PrecompileResult {
    // 1. Set up safe storage reference for this call frame
    let storage = StorageRef::new();

    // 2. Decode ABI input and dispatch to the appropriate handler
    let result = dispatch_precompile(address, input, gas_limit, &storage);

    // 3. On failure, revm's Journal rolls back EVM state (including any storage
    //    writes made through StorageRef) automatically. No separate snapshot needed.
    match &result {
        Ok(_) => commit_protocol_changes(),
        Err(_) => restore_protocol_snapshot(&snapshot),
    }

    result
}

/// Dispatch table for protocol precompiles
fn dispatch_precompile(address: Address, input: &[u8], gas_limit: u64, storage: &StorageRef) -> PrecompileResult {
    match address {
        0x101 => oracle_handler(input, gas_limit, storage),
        0x103 => bridge_handler(input, gas_limit, storage),
        0x201 => asset_handler(input, gas_limit, storage),   // transfer, batchTransfer, approve, mint, burn, register, registerErc20, createWrapper
        0x202 => shielded_handler(input, gas_limit, storage),
        0x203 => governance_handler(input, gas_limit, storage),
        0x204 => validator_handler(input, gas_limit, storage),
        0x205 => compliance_handler(input, gas_limit, storage),
        0x207 => switch_handler(input, gas_limit, storage),
        0x209 => agent_handler(input, gas_limit, storage),
        _ => Err(PrecompileError::NotFound),
    }
}

/// Example: transfer precompile handler (asset precompile at 0x201)
fn asset_transfer_handler(input: &[u8], gas_limit: u64, storage: &StorageRef) -> PrecompileResult {
    let decoded = decode_abi_transfer(input)?;
    let (asset_id, to, amount) = decoded;

    let sender = msg_sender(); // provided by revm call frame
    let asset = get_asset(asset_id, storage)?;

    check_compliance(&asset, sender, to, storage)?;
    transfer_balance(asset_id, sender, to, amount, storage)?;

    // Dynamic gas metering based on storage operations performed
    let gas_used = TRANSFER_BASE_GAS + storage.sload_count() * 50 + storage.sstore_count() * 500;

    Ok(PrecompileOutput {
        gas_used,
        bytes: encode_abi_bool(true),
    })
}
```

**Atomicity Guarantee:** Revm's Journal already guarantees that any EVM call frame (including precompiles) rolls back on failure. Protocol state lives in EVM storage under precompile addresses; writes made through `StorageRef` are subject to the same Journal rollback. No separate protocol-state snapshot is needed. Gas consumed by a failed precompile call is still charged (consistent with Ethereum semantics).

**Composability via Smart Contracts:** Because precompiles are ordinary EVM call targets, a smart contract can call multiple precompiles in a single transaction. Atomicity across multiple precompile calls within one contract invocation is guaranteed by revm's transaction-level Journal. For example, a payroll contract can loop over `0x201` (transfer) calls, and if any one fails, the entire contract call reverts.

### 3.7 EVM Gas Accounting for Precompile Calls

Protocol operations consume EVM gas according to standard Ethereum accounting. Each precompile call is an independent EVM call frame with its own gas limit and cost. There is no multi-precompile discount; gas is charged per call based on the precompile's fixed gas schedule.

**Precompile Gas Model (Dynamic Metering):**

All precompiles use dynamic gas metering based on actual storage operations:

```
gas_used = base_gas + sloads × 50 + sstores × 500
```

| Component | Cost | Description |
|-----------|------|-------------|
| Base gas | 2,100–30,000 | Per-precompile fixed overhead (varies by operation) |
| SLOAD | 50 gas | Per warm storage read |
| SSTORE | 500 gas | Per storage write |

Precompiles access EVM storage through `StorageRef`, which automatically tracks every `sload`/`sstore` and deducts gas according to Cancun rules. Revm's standard gas metering applies; unused gas is returned on success.

**Example:** An asset transfer precompile call at `0x201` that performs 2 SLOADs (read sender balance, read recipient balance) and 2 SSTOREs (write sender balance, write recipient balance) consumes `5,000 + 2×50 + 2×500 = 6,100 gas`.

### 3.8 Shielded Pool

The Shielded Pool provides protocol-level private transfer capability, hiding the sender, receiver, and amount. Implemented using zk-SNARKs, adopting Zcash's Note Commitment Tree + Nullifier model.

#### 3.8.1 Core Data Structures

```rust
/// Note -- represents an asset in the Shielded Pool
struct Note {
    value: u128,              // amount (encrypted state)
    asset_id: AssetId,
    rcm: Scalar,              // random number
    recipient_view_key: PublicKey,  // recipient view key
}

/// Note commitment -- placed in global Merkle Tree
struct NoteCommitment(pub Hash);

/// Nullifier -- prevents double-spending
struct Nullifier(pub Hash);

/// View key -- selective disclosure
struct ViewingKey {
    incoming_view_key: PublicKey,  // view incoming
    full_view_key: PublicKey,      // view all related transactions
}

/// ZK proof -- proves transaction is valid without revealing details
struct ZkProof {
    proof_data: Vec<u8>,         // Halo2 IPA proof
    public_inputs: PublicInputs,  // public nullifiers + commitments
}
```

#### 3.8.2 Three Operations

```
Deposit (ShieldedDeposit):
  Transparent address → Shielded Pool
  1. Deduct from sender's transparent balance
  2. Generate Note and Commitment
  3. Add Commitment to Merkle Tree

  Externally visible: An address deposited assets into the pool
  Externally hidden: How much was deposited, ultimate ownership

Shielded Transfer (ShieldedTransfer):
  Shielded Pool → Shielded Pool
  1. Select existing Notes as inputs
  2. Create new Notes as outputs
  3. Generate ZK proof:
     - Input Notes actually exist (Merkle proof)
     - Input Notes not yet spent (nullifier not in set)
     - Output total <= Input total (no counterfeiting)
     - Sender owns spending key for input Notes
  4. Reveal nullifiers (mark inputs as spent)
  5. New commitments added to Merkle Tree

  Externally visible: Someone performed a shielded transfer
  Externally hidden: Who sent it, to whom, how much

Withdrawal (ShieldedWithdraw):
  Shielded Pool → Transparent address
  1. Consume Shielded Note
  2. Generate ZK proof
  3. Amount publicly revealed to transparent balance

  Externally visible: Someone withdrew amount X from pool to address A
  Externally hidden: Original identity of the withdrawer
```

#### 3.8.3 ZK Circuit

```rust
/// ZK circuit for ShieldedTransfer (proof statement)
///
/// Public inputs:
///   - nullifiers[]          (spent inputs)
///   - commitments[]         (new outputs)
///   - asset_id
///
/// Private inputs:
///   - notes[]               (consumed Notes)
///   - new_notes[]           (newly created Notes)
///   - spending_key          (sender private key)
///   - merkle_path[]         (Merkle proof path)
///
/// Constraints:
///   1. Each note's nullifier correctly derived
///   2. Each note is in Merkle Tree (path valid)
///   3. Sender has spending rights for notes
///   4. sum(new_notes.value) <= sum(notes.value)
///   5. All values in valid range (no overflow/underflow)
```

#### 3.8.4 Merkle Tree State

```rust
/// One independent Merkle Tree per asset
/// or shared tree with asset_id encoded in Note
struct ShieldedState {
    merkle_tree: SparseMerkleTree<NoteCommitment>,
    nullifier_set: HashSet<Nullifier>,  // spent nullifiers
    note_registry: HashMap<NoteCommitment, EncryptedNote>,  // on-chain storage of encrypted Notes
}
```

The Merkle Tree uses an **Incremental Merkle Tree** with depth 32, supporting approximately 4.2 billion leaf nodes. Each new commitment only requires O(log n) update.

#### 3.8.5 Compliance and Auditing

The Shielded Pool has built-in compliance support; it is not an "untraceable darknet tool":

```rust
enum ShieldedComplianceMode {
    /// Full privacy, no mandatory disclosure
    Unrestricted,
    /// Receiver must hold valid KYC mark
    KycRequired,
    /// Asset issuer can audit via viewing key
    IssuerAuditable,
    /// Only allow shielded transfers between whitelisted addresses
    WhitelistedOnly,
}
```

**Audit Process:**
1. User generates a viewing key for the auditor
2. Auditor uses the viewing key to decrypt relevant transactions
3. Verify compliance, not exposed to the public

#### 3.8.6 Proof System Selection

| Scheme | Proof Size | Verification Time | Trusted Setup | Recommendation |
|------|---------|---------|---------|--------|
| Halo2 IPA | ~5-10KB | ~5-10ms | Not required | Current — no trusted setup |
| Halo2 KZG | ~1KB | ~8ms | Universal SRS | Future option |
| Plonk | ~1KB | ~8ms | Required (universal) | Backup option |

Adoption of **Halo2 IPA** (no trusted setup, recursive composition ready).

#### 3.8.7 Performance Impact

| Metric | Value |
|------|------|
| Proof generation time | 2-5 seconds (client-side local) |
| Proof verification time | ~5-10ms/transaction (on-chain) |
| Proof data size | ~5-10 KB (Halo2 IPA) |
| Nullifier check | O(1) via HashSet |
| Merkle Tree update | O(log n), depth 32 |

With 21 validators per round subset, assuming 100 Shielded transactions:
- Total verification time: 100 × 3ms = 300ms
- May become a bottleneck within 250ms block time
- **Solution**: Cap at 50 Shielded transactions per block, excess queued for next block

### 3.9 Smart Accounts

Callchain uses standard Ethereum transaction signing (secp256k1) for all transactions. Multi-signature and session-key functionality are supported via smart contract accounts (ERC-4337 style account abstraction) or via protocol precompiles that enforce additional authorization checks. Gas sponsorship is handled at the transaction level through EVM gas fields or sponsor precompiles.

```
Authentication (who signs)       Gas Payment (who pays)
├── EOA (standard secp256k1)     ├── SelfPay (tx.sender pays)
├── MultiSig (contract account)  ├── AuthorizedSponsor (pre-authorized)
└── SessionKey (contract account)├── PoolSponsor (pre-deposit pool)
                                 └── PerTxSponsor (per-tx signature)
```

#### 3.9.1 Authentication Schemes

All transactions are standard Ethereum transactions signed with secp256k1. Additional authentication logic is implemented via smart contract accounts or precompile-level checks.

```rust
/// Standard Ethereum transaction signature (used for all tx types)
struct EcdsaSignature {
    v: u64,
    r: U256,
    s: U256,
}

/// Multi-signature verification is performed by a smart contract account
/// or by the multi-sig precompile at 0x110 (if implemented natively).
/// The transaction sender is the contract address; the contract validates
/// that sufficient signers have approved the operation.

/// Session Key authorization is managed by a smart contract wallet
/// or by the session-key precompile at 0x111. The owner registers
/// session keys with permissions, expiry, and spending limits.
```

#### 3.9.2 Multi-Signature Account (m-of-n)

Multi-signature accounts allow distributing account control across multiple keys. After registering a multi-sig configuration, every transaction from that account requires signatures from at least `threshold` signers.

```rust
/// Multi-sig configuration -- bound to account address
struct MultiSigConfig {
    signers: Vec<Address>,          // n signers
    threshold: u8,                  // at least m signatures required (m ≤ n)
    version: u64,                   // version number (for configuration rotation)
}
```

**Registration and Changes:**

```rust
fn register_multi_sig(
    new_address: Address,           // new multi-sig address
    config: MultiSigConfig,
    // Note: registration requires all signers to sign, to prevent malicious registration
) -> Result<()> {
    ensure!(config.threshold <= config.signers.len() as u8,
            "Threshold exceeds signers");
    ensure!(config.threshold >= 1, "Threshold must be at least 1");
    ensure!(config.signers.len() >= 2 && config.signers.len() <= 10,
            "Signer count must be 2-10");

    // Verify all signers have signed the registration consent
    for signer in &config.signers {
        ensure!(signer_signed_registration(signer, new_address),
                "Signer did not consent");
    }

    MultiSigConfigs::insert(new_address, config);
    Ok(())
}

fn update_multi_sig(
    account: Address,
    new_config: MultiSigConfig,
    // Requires old config's threshold number of signatures to authorize change
    authorizations: Vec<Signature>,
) -> Result<()> {
    let old_config = MultiSigConfigs::get(account).ok_or("Not multi-sig")?;
    ensure!(authorizations.len() >= old_config.threshold as usize,
            "Insufficient authorizations");

    // Verify signatures come from old signers
    let valid_count = verify_signatures_from_signers(
        &authorizations, &old_config.signers
    )?;
    ensure!(valid_count >= old_config.threshold as usize,
            "Invalid authorizations");

    MultiSigConfigs::insert(account, new_config);
    Ok(())
}
```

**Verification Logic:**

```rust
fn verify_multisig(account: Address, signatures: &[Signature]) -> Result<()> {
    let config = MultiSigConfigs::get(account)
        .ok_or("No multi-sig config")?;

    ensure!(signatures.len() >= config.threshold as usize,
            "Not enough signatures");

    // Count how many signatures come from legitimate signers
    let valid_count = signatures.iter()
        .filter(|sig| {
            let signer = recover_signer(&sig.hash());
            config.signers.contains(&signer)
        })
        .count();

    ensure!(valid_count >= config.threshold as usize,
            "Invalid threshold");

    // Additional check: deduplicate to prevent same signer signing multiple times
    ensure!(signatures.len() <= config.signers.len(),
            "Duplicate signer detected");

    Ok(())
}
```

**Typical Use Cases:**

| Scenario | Configuration | Description |
|------|------|------|
| Team treasury | 3-of-5 | 5 core members, any 3 can operate |
| DAO multi-sig | 5-of-9 | 9 council members, 5 majority can decide |
| Family account | 2-of-3 | Self + spouse + lawyer, any 2 |
| Corporate approval | 2-of-4 | CEO + CFO + Director + Legal, any 2 |

#### 3.9.3 Social Recovery Wallet

When a user loses their private key, they can recover account control through a pre-configured Guardian network, without relying on mnemonic phrase backups.

```rust
/// Social recovery configuration
struct SocialRecoveryConfig {
    guardians: Vec<Address>,            // 3-10 guardians
    threshold: u8,                      // minimum number of approvals required (recommended: threshold = ceil(guardians/2 + 1))
    recovery_delay_secs: u64,           // delay before taking effect (24-72 hours)
    pending_recovery: Option<RecoveryRequest>,
}

/// Pending recovery request
struct RecoveryRequest {
    new_key: Address,                   // new primary key
    approved_by: Vec<Address>,          // Guardians who have approved
    initiated_at: u64,                  // initiation timestamp
    initiator_signature: Option<Signature>, // old key holder signature (if old key still available)
}
```

**Recovery Process:**

```
1. Initiate recovery request
   → User (with old key or other method) submits RecoveryRequest
   → Specifies new key new_key
   → Request enters on-chain state

2. Guardian voting
   → Guardians sign one by one to confirm
   → Each confirmation adds to approved_by
   → Reaching threshold Guardian approvals → enters delay period

3. Delay period (24-72 hours)
   → If user still has old key, they can unilaterally cancel the recovery
   → Prevents Guardian collusion attacks
   → Delay period starts when threshold is reached

4. Delay expires → automatically takes effect
   → Old key becomes invalid
   → New key becomes account primary key
   → SocialRecoveryConfig clears pending_recovery
```

```rust
fn initiate_recovery(
    account: Address,
    new_key: Address,
    old_key_signature: Option<Signature>,  // optional, proves identity
) -> Result<()> {
    let config = SocialRecoveryConfigs::get(account)
        .ok_or("No recovery config")?;

    ensure!(config.guardians.len() >= 3, "Need at least 3 guardians");
    ensure!(new_key != Address::zero(), "Invalid new key");

    let request = RecoveryRequest {
        new_key,
        approved_by: vec![],
        initiated_at: current_timestamp(),
        initiator_signature: old_key_signature,
    };
    config.pending_recovery = Some(request);
    SocialRecoveryConfigs::insert(account, config);
    emit_event("RecoveryInitiated", account, new_key);
    Ok(())
}

fn guardian_approve(
    account: Address,
    guardian: Address,
    guardian_signature: Signature,
) -> Result<()> {
    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    let request = config.pending_recovery.as_mut()
        .ok_or("No pending recovery")?;

    // Verify guardian identity
    ensure!(config.guardians.contains(&guardian), "Not a guardian");

    // Verify guardian signature
    verify_signature(&guardian, &guardian_signature, request.new_key)?;

    // Prevent duplicates
    ensure!(!request.approved_by.contains(&guardian), "Already approved");

    request.approved_by.push(guardian);

    // Threshold reached, set delay生效
    if request.approved_by.len() >= config.threshold as usize {
        let effective_time = current_timestamp() + config.recovery_delay_secs;
        emit_event("RecoveryApproved", account, effective_time);
    }

    Ok(())
}

fn finalize_recovery(account: Address) -> Result<()> {
    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    let request = config.pending_recovery.take()
        .ok_or("No pending recovery")?;

    // Check Guardian threshold
    ensure!(request.approved_by.len() >= config.threshold as usize,
            "Insufficient guardian approvals");

    // Check delay period has passed
    let elapsed = current_timestamp() - request.initiated_at;
    ensure!(elapsed >= config.recovery_delay_secs,
            "Recovery delay not met");

    // Replace account primary key
    AccountKeys::insert(account, request.new_key);

    // Clear pending state
    config.pending_recovery = None;
    SocialRecoveryConfigs::insert(account, config);

    emit_event("RecoveryFinalized", account, request.new_key);
    Ok(())
}

fn cancel_recovery(account: Address, owner_signature: Signature) -> Result<()> {
    // Account owner can cancel at any time during the delay period
    verify_signature(&account, &owner_signature, "cancel_recovery")?;

    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    config.pending_recovery = None;
    SocialRecoveryConfigs::insert(account, config);

    emit_event("RecoveryCancelled", account);
    Ok(())
}
```

**Guardian Type Recommendations:**

| Guardian Type | Example | Recommended Count |
|--------------|------|---------|
| Own devices | Backup phone, hardware wallet, laptop | 1-2 |
| Trusted persons | Spouse, family member, close friend | 1-2 |
| Professional institutions | Law firm, bank, custody service | 0-1 |
| Time lock | Time-based auto-recovery (backup) | 0-1 |

Recommended configuration: 5 Guardians, threshold 3, delay 48 hours.

#### 3.9.4 Session Key

Session Keys are temporary keys used to authorize dApps or Agents to operate within defined scopes, without requiring the user to sign every time.

```rust
/// Session Key configuration
struct SessionKeyConfig {
    key: Address,                       // Session Key public key address
    permissions: SessionPermissions,
    expires_at: u64,                    // expiry timestamp
    created_at: u64,
}

/// Session Key permissions
struct SessionPermissions {
    /// Allowed precompile addresses (empty = all addresses)
    allowed_precompiles: Vec<Address>,
    /// Maximum amount per transaction (0 = unlimited)
    max_per_tx: u128,
    /// Daily cumulative amount limit (0 = unlimited)
    max_daily: u128,
    /// Allowed target addresses for interaction (empty = any)
    allowed_targets: Vec<Address>,
    /// Allowed assets to operate (empty = all)
    allowed_assets: Vec<AssetId>,
}
```

**Creation and Revocation:**

```rust
fn create_session_key(
    account: Address,
    session_key: Address,
    permissions: SessionPermissions,
    duration_secs: u64,
    signature: Signature,           // account owner signature
) -> Result<()> {
    verify_signature(&account, &signature, session_key)?;

    let config = SessionKeyConfig {
        key: session_key,
        permissions,
        expires_at: current_timestamp() + duration_secs,
        created_at: current_timestamp(),
    };
    SessionKeys::insert(account, session_key, config);
    Ok(())
}

fn revoke_session_key(
    account: Address,
    session_key: Address,
    signature: Signature,
) -> Result<()> {
    verify_signature(&account, &signature, session_key)?;
    SessionKeys::remove(account, session_key);
    Ok(())
}
```

**Verification Logic:**

```rust
fn verify_session_key(account: Address, key: Address, signature: &Signature, tx_hash: &Hash, call_data: &[u8]) -> Result<()> {
    // 1. Verify Session Key signature
    verify_signature(&key, signature, tx_hash)?;

    // 2. Retrieve configuration
    let config = SessionKeys::get(account, key)
        .ok_or("Session key not found")?;

    // 3. Verify not expired
    ensure!(current_timestamp() < config.expires_at,
            "Session key expired");

    // 4. Verify permissions against the precompile call being made
    let perms = &config.permissions;
    let (precompile_addr, decoded) = decode_precompile_call(call_data)?;

    if !perms.allowed_precompiles.is_empty() {
        ensure!(perms.allowed_precompiles.contains(&precompile_addr),
                "Precompile not allowed");
    }
    if perms.max_per_tx > 0 {
        let amount = decoded.amount().unwrap_or(0);
        ensure!(amount <= perms.max_per_tx,
                "Exceeds per-tx limit");
    }
    if perms.max_daily > 0 {
        let today = current_timestamp() / 86400;
        let daily_spent = SessionKeyDailyUsage::get(account, key, today);
        let tx_amount = decoded.amount().unwrap_or(0);
        ensure!(daily_spent + tx_amount <= perms.max_daily,
                "Exceeds daily limit");
        SessionKeyDailyUsage::insert(account, key, today, daily_spent + tx_amount);
    }
    if !perms.allowed_targets.is_empty() {
        if let Some(target) = decoded.target() {
            ensure!(perms.allowed_targets.contains(&target),
                    "Target not allowed");
        }
    }
    if !perms.allowed_assets.is_empty() {
        if let Some(asset_id) = decoded.asset_id() {
            ensure!(perms.allowed_assets.contains(&asset_id),
                    "Asset not allowed");
        }
    }

    Ok(())
}
```

**Typical Use Cases:**

| Scenario | Permission Settings | Validity |
|------|---------|--------|
| dApp gaming | Only Transfer, per-tx < 10 CALL, daily < 100 CALL | 24 hours |
| Agent auto-pay | AgentPay + AgentCall, per-tx < 50 USDC | 7 days |
| DeFi strategy bot | Only Approve + TransferFrom, target = specified DEX | 1 hour |
| Wallet preview mode | Only view operations (no signature needed) | Permanent |

#### 3.9.5 Unified Authentication Flow

All transactions use standard secp256k1 ECDSA recovery to derive the sender address. Additional authentication (multi-sig, session keys) is enforced by the sender contract or by precompile-level checks.

```rust
/// Standard EVM sender recovery (used for every transaction)
fn recover_sender(tx: &EvmTx) -> Result<Address> {
    let msg_hash = keccak256(encode_tx_for_signing(tx));
    let pubkey = recover_secp256k1(&tx.signature, &msg_hash)?;
    Ok(pubkey_to_address(&pubkey))
}

/// For contract accounts (multi-sig, session-key wallets), the transaction
/// sender is the contract address. The contract's `validateUserOp` or
/// equivalent entrypoint performs additional signer verification.

/// For EOAs, verify no pending social recovery has replaced the key
fn verify_eoa_sender(account: Address) -> Result<()> {
    if let Some(config) = SocialRecoveryConfigs::get(account) {
        if let Some(request) = &config.pending_recovery {
            if request.approved_by.len() >= config.threshold as usize {
                let elapsed = current_timestamp() - request.initiated_at;
                if elapsed >= config.recovery_delay_secs {
                    return Err("Account key has been recovered");
                }
            }
        }
    }
    Ok(())
}
```

#### 3.9.6 Storage Structure

```
callchain/
├── accounts/
│   ├── {address}/
│   │   ├── key                     # primary key (SingleSig)
│   │   ├── multi_sig_config        # multi-sig configuration (if set)
│   │   ├── social_recovery_config  # social recovery configuration (if set)
│   │   └── session_keys/           # Session Keys (0-N)
│   │       └── {session_key}/      # each Session Key
│   │           ├── config          # permissions + expiry
│   │           └── daily_usage     # daily usage total
└── ...
```

Storage estimates:
- Multi-sig account: ~200 bytes (10 signers × 20 bytes + metadata)
- Social recovery: ~300 bytes (5 Guardians + pending recovery state)
- Session Key: ~100 bytes each (permissions + expiry)

#### 3.9.7 Synergy with Agent Model

Session Keys and Agent accounts are different abstraction layers:

| Feature | Session Key | Agent Account |
|------|------------|-----------|
| Lifecycle | Temporary (hours/days) | Long-term (months/years) |
| Permission Granularity | Precompile-level, amount limits | Asset-level, counterparty limits |
| Fund Ownership | User primary account | Independent sub-account (allocated from Owner) |
| Fee Payment | Typically SelfPay | Typically AuthorizedSponsor (Owner pays) |
| Applicable Scenarios | dApp interaction, temporary authorization | AI Agent long-term auto-pay |

Can be combined: Agent transactions can be initiated with Session Keys (short-term authorization), while Owner sponsors Gas.

---

## 4. EVM Smart Contract Layer

### 4.1 Execution Engine

Based on **Reth + Revm**, embedded as a library in the same process.

- Fully compatible with Ethereum EVM
- Supports all standard Ethereum JSON-RPC methods
- Compatible with Foundry / Hardhat development toolchain

### 4.2 ERC-20 Contracts for Assets

Only protocol assets with `has_erc20 = 1` have a corresponding ERC-20 contract on the EVM layer. There are two binding models depending on `dominance`:

**Escrow model (`dominance = 0`, EVM-dominant):**
- Used for external ERC-20 contracts registered via `registerErc20(address)`.
- The Switch precompile (`0x207`) holds tokens in its own balance as escrow.
- Switching protocol → EVM uses `transfer(to, amount)` from the escrow.
- Switching EVM → protocol uses `transferFrom(caller, 0x207, amount)` into escrow.

**Mint/burn model (`dominance = 1`, PROTOCOL-dominant):**
- Used for system `WrappedToken` contracts deployed via `createWrapper(assetId)`.
- No escrow is needed. The Switch precompile mints/burns tokens on demand.
- `bridgeMint(to, amount)` and `bridgeBurn(from, amount)` are restricted to `msg.sender == 0x207`.

Protocol-only assets (`has_erc20 = 0`) cannot leave the protocol layer until an issuer binds them via one of the two paths above.

```solidity
/// System WrappedToken for PROTOCOL-dominant assets (dominance = 1)
/// Deployed via createWrapper(assetId) at 0x201
contract WrappedToken is IERC20 {
    uint8 public immutable decimals;
    string public name;
    string public symbol;
    uint256 public totalSupply;
    address public immutable bridge;      // Switch precompile 0x207

    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    modifier onlyBridge() {
        require(msg.sender == bridge, "Only bridge");
        _;
    }

    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);

    function bridgeMint(address to, uint256 amount) external onlyBridge {
        _balances[to] += amount;
        totalSupply += amount;
    }

    function bridgeBurn(address from, uint256 amount) external onlyBridge {
        _balances[from] -= amount;
        totalSupply -= amount;
    }
}
```

### 4.3 Independent ERC-20 Contracts

In addition to ERC-20 contracts corresponding to protocol assets, the EVM layer also supports:
- Deployment of any standard ERC-20 contracts
- Non-protocol asset tokens (meme coins, governance tokens, etc.)
- Standard DeFi contracts (DEX, Lending, ...) without any adaptation
- EVM gas uses dynamic pricing

**Key: The EVM layer is a fully functional Ethereum chain, and protocol assets participate in DeFi through standard ERC-20 interfaces.**

---

## 5. Internal Bridge (Switch Precompile)

### 5.1 Design

Protocol-layer balances and EVM-layer balances are **two independent ledgers**. The **Switch precompile at `0x207`** provides bidirectional bridging between them. All state changes are atomic via checkpoint/rollback.

Two models are supported depending on the asset's `dominance`:

- **Escrow model** (`dominance = 0`, EVM-dominant): The Switch precompile holds ERC-20 tokens in its own balance. Used for external contracts registered via `registerErc20`.
- **Mint/burn model** (`dominance = 1`, PROTOCOL-dominant): The Switch precompile creates or destroys ERC-20 tokens in real time via `bridgeMint`/`bridgeBurn`. Used for system wrappers deployed via `createWrapper`.

CALL (asset_id = 1) is a special case: it bridges as **native EVM gas balance** (`evm_state.set_balance`), not as an ERC-20.

### 5.2 Switch Precompile Interface

```solidity
interface IProtocolSwitch {
    function switchToEvm(uint64 assetId, address to, uint128 amount) external;
    function switchToProtocol(uint64 assetId, address to, uint128 amount) external;
}
```

Both functions are called via standard EVM transactions to `0x207`.

### 5.3 SwitchToEvm (Protocol → EVM)

**Flow:**
1. Validate `amount > 0` and `to != Address::ZERO`
2. `check_asset_active(assetId)` — asset must be Active
3. `check_has_erc20(assetId)` — must support EVM bridge (or `assetId == 1` for CALL)
4. `sub_protocol_bal(assetId, caller, amount)` — deduct caller's protocol balance
5. Credit EVM side:
   - If `assetId == 1` (CALL): `balance_add(to, amount)` — native EVM balance
   - Otherwise (ERC-20): read `dominance`, execute nested EVM call from `0x207`
     - `dominance == 0`: `transfer(to, amount)` from escrow
     - `dominance == 1`: `bridgeMint(to, amount)` — mint new tokens
6. `checkpoint_commit()` on success; `checkpoint_revert()` on any failure

**Gas:** 20,000 + nested EVM call gas

### 5.4 SwitchToProtocol (EVM → Protocol)

**Flow:**
1. Validate `amount > 0` and `to != Address::ZERO`
2. `check_asset_active(assetId)`
3. `check_has_erc20(assetId)`
4. Deduct EVM side:
   - If `assetId == 1` (CALL): `balance_sub(caller, amount)` — native EVM balance
   - Otherwise (ERC-20): read `dominance`, execute nested EVM call from `0x207`
     - `dominance == 0`: `transferFrom(caller, 0x207, amount)` into escrow
     - `dominance == 1`: `bridgeBurn(caller, amount)` — destroy caller's tokens
5. `add_protocol_bal(assetId, to, amount)` — credit recipient's protocol balance
6. Atomic commit or rollback

**Gas:** 20,000 + nested EVM call gas

### 5.5 Atomicity

Both methods execute inside `dispatch::mutate_void`:

```rust
let checkpoint = storage.checkpoint();
let result = handler(decoded, storage);
if result.is_ok() && gas_sufficient {
    storage.checkpoint_commit(checkpoint);
} else {
    storage.checkpoint_revert(checkpoint);
}
```

This means:
- `switchToEvm`: if protocol balance was deducted but ERC-20 call fails → **all state restored**
- `switchToProtocol`: if ERC-20 call succeeds but protocol balance credit fails → **all state restored**

### 5.6 Liquidity Requirement

**Escrow model (`dominance = 0`):** The Switch precompile (`0x207`) must hold a token balance before any `switchToEvm` deposit can succeed. Liquidity is injected through user-initiated withdrawals (`switchToProtocol`) or direct `transfer` to `0x207`.

**Mint/burn model (`dominance = 1`):** No escrow balance is required. Tokens are created on demand during `switchToEvm` and destroyed during `switchToProtocol`.

---

## 5.6 External Cross-Chain Bridge (External Bridge)

Callchain implements cross-chain bridging through **validator consensus subset signatures**, without relying on third-party bridges or BLS aggregate signatures. Validators use their existing secp256k1 consensus keys, utilizing 2/3 of the 21-validator per-round subset (i.e., 14 signatures) to complete bridge verification.

### 5.6.1 Trust Model

```
21 validators per round (randomly selected from 100-216)
    │
    ├── 2/3 threshold = 14 signatures
    ├── Validators sign with secp256k1 keys
    ├── Signatures verified at Callchain protocol layer (deposit direction)
    └── Signatures verified at Ethereum contract (withdrawal direction)

Security:
  Bridge security ≤ Consensus security
  If 14/21 validators collude → the chain itself is also insecure
```

### 5.6.2 Bridge Data Structures

```rust
/// External chain identifier
enum ExternalChain {
    EthereumMainnet,     // Ethereum mainnet
    Arbitrum,            // Arbitrum
    // Extensible
}

/// External bridge operation
enum ExternalBridgeOp {
    /// External chain → Callchain (deposit)
    Deposit {
        source_chain: ExternalChain,
        source_tx_hash: Hash,
        source_block_number: u64,
        sender: Vec<u8>,              // external chain address (raw bytes)
        recipient: Address,           // Callchain receiving address
        asset_id: AssetId,
        amount: u128,
        signatures: Vec<Signature>,   // 14+ validator signatures
    },
    /// Callchain → External chain (withdrawal)
    Withdraw {
        target_chain: ExternalChain,
        target_address: Vec<u8>,      // external chain receiving address
        asset_id: AssetId,
        sender: Address,
        amount: u128,
    },
}
```

### 5.6.3 Deposit Flow (External Chain → Callchain)

```
User operates on Ethereum:
  1. Calls Ethereum bridge contract deposit(assetId, amount, callchainRecipient)
  2. Ethereum contract locks assets, triggers DepositInitiated event

Callchain validator operations:
  3. Each validator runs bridge monitoring process, detects Ethereum DepositInitiated event
  4. Waits min_confirmations block confirmations (Ethereum default 12)
  5. After confirmation, each validator signs deposit_hash with secp256k1 private key
  6. Signatures propagate through P2P network, aggregate after collecting 14 signatures
  7. Any node submits ExternalBridgeDeposit transaction to Callchain
     (includes 14 signatures + deposit data)
  8. Callchain protocol layer verifies 14 signatures, mints corresponding asset after verification
```

### 5.6.4 Withdrawal Flow (Callchain → External Chain)

```
User operates on Callchain:
  1. Initiates ExternalBridgeWithdraw precompile call
  2. Callchain burns corresponding asset
  3. Withdrawal request added to pending queue

Validator operations:
  4. Validators collect withdrawal requests when packing blocks
  5. Each validator signs the withdrawal data
  6. After collecting 14 signatures, aggregate and submit to Ethereum bridge contract
  7. Ethereum contract verifies 14 signatures, releases asset after verification
```

### 5.6.5 Bridge Signature Verification

```rust
/// Verify a set of bridge signatures
fn verify_bridge_signatures(
    message_hash: Hash,
    signatures: &[Signature],
    min_signatures: u8,
) -> Result<()> {
    ensure!(signatures.len() >= min_signatures as usize,
            "Insufficient bridge signatures");

    // Get current active validators
    let validators = get_active_validators();
    let required = validators.len() * 2 / 3 + 1;  // 2/3 + 1

    let mut unique_validators = HashSet::new();

    for sig in signatures {
        let signer = recover_secp256k1_signer(message_hash, sig)?;
        if validators.contains(&signer) {
            unique_validators.insert(signer);
        }
    }

    ensure!(unique_validators.len() >= required,
            "Not enough valid validator signatures");

    Ok(())
}

/// Validator bridge signature service
fn sign_bridge_event(
    private_key: &SecretKey,
    event: &BridgeEvent,
) -> Signature {
    let deposit_hash = keccak256(&abi_encode(
        event.source_chain,
        event.source_tx_hash,
        event.asset_id,
        event.sender,
        event.recipient,
        event.amount,
    ));
    sign_secp256k1(private_key, &deposit_hash)
}
```

### 5.6.6 Ethereum-Side Bridge Contract

```solidity
contract CallchainBridge {
    /// Validator public key registration
    mapping(address => bool) public validators;
    uint256 public validatorCount;

    /// Deposit: user deposits from Ethereum to Callchain
    function deposit(
        uint256 assetId,
        uint256 amount,
        bytes32 callchainRecipient
    ) external {
        require(amount > 0, "Invalid amount");
        IERC20(assetId).transferFrom(msg.sender, address(this), amount);
        emit DepositInitiated(assetId, amount, callchainRecipient, msg.sender);
    }

    /// Withdrawal: withdraw from Callchain to Ethereum
    /// Requires 14+ validator signatures
    function withdraw(
        uint256 assetId,
        uint256 amount,
        address recipient,
        bytes32 sourceTxHash,
        bytes[] calldata signatures  // 14+ secp256k1 signatures
    ) external {
        // 1. Anti-replay
        require(!processedWithdraws[sourceTxHash], "Already processed");
        processedWithdraws[sourceTxHash] = true;

        // 2. Build signed message
        bytes32 messageHash = keccak256(abi.encode(
            assetId, amount, recipient, sourceTxHash, address(this)
        ));

        // 3. Verify signatures
        uint256 validCount;
        for (uint256 i = 0; i < signatures.length; i++) {
            address signer = recoverSigner(messageHash, signatures[i]);
            require(validators[signer], "Invalid validator signature");
            validCount++;
        }
        require(validCount >= getQuorum(), "Insufficient signatures");

        // 4. Release assets
        IERC20(assetId).transfer(recipient, amount);
    }

    function getQuorum() public view returns (uint256) {
        return (validatorCount * 2) / 3 + 1;
    }
}
```

### 5.6.7 Bridge Security Limits

```rust
struct BridgeConfig {
    /// Maximum bridge amount per transaction
    max_per_tx: u128,

    /// Daily total bridge limit (per asset)
    daily_limit_per_asset: u128,

    /// Ethereum minimum confirmations
    eth_min_confirmations: u32,       // default 12

    /// Bridge fee (covers Ethereum gas costs)
    bridge_fee: u128,

    /// Whitelisted assets (only registered external assets allowed)
    allowed_assets: Vec<AssetId>,

    /// Signature collection timeout (prevents bridge from stalling)
    signature_timeout_secs: u64,      // default 300 seconds
}
```

| Parameter | Default Value | Description |
|------|--------|------|
| max_per_tx | 1,000,000 USDC | Anti-whale attack |
| daily_limit_per_asset | 10,000,000 USDC | Anti-large capital flow shock |
| eth_min_confirmations | 12 | Ethereum standard confirmations |
| signature_timeout_secs | 300 | Signature collection timeout, auto-retry |

---

## 6. Agent Payments

### 6.1 Agent Account Model

Agent accounts do not hold independent balances; instead, they are allocated from the owner's account with authorization. The owner deposits funds into the Agent sub-account, and the Agent can only operate within its permission scope.

```rust
struct AgentRegistration {
    agent_id: u64,                    // unique Agent ID assigned by protocol
    owner: Address,                   // fund owner (Owner)
    agent_public_key: PublicKey,      // Agent operation key (secp256k1)
    name: String,                     // display name
    url: Option<String>,              // public info URL
    metadata_hash: Option<Hash>,      // Agent code/configuration hash
    domain_proof: Option<DomainProof>, // domain ownership proof (optional)
    registered_at: u64,
}

enum DomainProof {
    /// Place verification file at specified domain's .well-known/callchain-agent
    DnsTxt { domain: String, txt_value: String },
    /// Access domain verification endpoint via HTTP
    HttpFile { url: String, expected_content: String },
}
```

### 6.2 Agent Permissions and Fee Configuration

```rust
struct AgentPermissions {
    allowed_assets: Vec<AssetId>,    // list of allowed assets (empty = all)
    daily_limit: u128,               // daily spending cap (0 = unlimited)
    per_tx_limit: u128,             // per-transaction cap (0 = unlimited)
    allowed_counterparties: Vec<Address>, // whitelist (empty = any)
    allowed_protocols: Vec<Address>,     // allowed EVM contracts for interaction (empty = any)
    expires_at: u64,                     // expiry timestamp (0 = never expires)
}

struct AgentFeeConfig {
    fee_payer: FeePayer,
    owner_max_daily_fee: u128,      // maximum daily fee the Owner pays on behalf of Agent
    owner_max_total_fee: u128,       // maximum cumulative fee the Owner pays on behalf of Agent
    require_owner_signature_above: u128,  // amounts above this require Owner's second signature
}

enum FeePayer {
    /// Agent sub-account pays fees from its balance
    SelfPay,
    /// Owner primary account pays fees on behalf
    OwnerPays,
    /// Third-party pays (platform/protocol subsidy)
    ThirdParty { payer: Address },
}
```

**Owner-sponsored payment is the recommended mode for Agent payments.** The Agent submits standard EVM transactions calling the Agent precompile (`0x209`), and Gas is deducted from the Owner account via an authorized sponsor precompile or ERC-4337 paymaster. The Owner signs once to authorize, and the Agent's subsequent transactions do not require the Owner to sign again. Agents can submit multiple Agent operations in separate calls within the same block or batch them via a smart contract.

### 6.3 Agent Fund Authorization

```rust
enum AgentFundingAction {
    /// Owner creates and authorizes Agent
    Grant {
        owner: Address,
        agent_public_key: PublicKey,
        name: String,
        url: Option<String>,
        domain_proof: Option<DomainProof>,
        amount: u128,                // initial authorized amount
        asset_id: AssetId,
        permissions: AgentPermissions,
        fee_config: AgentFeeConfig,
    },
    /// Top up funds
    TopUp {
        agent_id: u64,
        amount: u128,
        asset_id: AssetId,
    },
    /// Immediate revocation, remaining funds returned to Owner
    Revoke {
        agent_id: u64,
    },
    /// Update permissions or fee configuration
    UpdateConfig {
        agent_id: u64,
        new_permissions: Option<AgentPermissions>,
        new_fee_config: Option<AgentFeeConfig>,
    },
}
```

**Revoke and UpdateConfig are initiated directly by the Owner, do not require Agent cooperation, and take effect immediately.**

### 6.4 Agent Balance Management

```rust
/// Agent sub-account balances
/// (owner_address, agent_id, asset_id) → balance
type AgentBalances = HashMap<(Address, u64, AssetId), u128>;

/// Agent nonce (anti-replay)
/// (owner_address, agent_id) → nonce
type AgentNonces = HashMap<(Address, u64), u64>;
```

### 6.5 Agent Precompile Operations

Agent operations are invoked via the Agent precompile at address `0x209`. Agents submit standard EVM transactions calling this precompile with ABI-encoded arguments. The precompile verifies Agent permissions, deducts funds from the Agent sub-account, and executes the requested operation.

**Supported Agent precompile functions:**

```solidity
interface IAgentPrecompile {
    function agentPay(uint64 agentId, uint64 assetId, address to, uint128 amount) external returns (bool);
    function agentBatchPay(uint64 agentId, uint64 assetId, address[] calldata tos, uint128[] calldata amounts) external returns (bool);
    function agentCall(uint64 agentId, uint64 assetId, address contract, bytes calldata data, uint128 value) external returns (bytes memory);
    function agentBridgeDeposit(uint64 agentId, uint64 assetId, address to, uint128 amount) external returns (bool);
}
```

**Example: Agent payment via precompile**

```rust
let tx = EvmTx {
    to: Address::from_hex("0x209"),          // Agent precompile
    data: encode_abi("agentPay", &[
        Token::Uint(agent_id.into()),
        Token::Uint(asset_id.into()),
        Token::Address(recipient),
        Token::Uint(amount.into()),
    ]),
    value: 0,
    gas_limit: 21_000,
    max_fee_per_gas: 10_000_000_000,
    max_priority_fee_per_gas: 1_000_000_000,
    nonce: agent_nonce,
    chain_id: 1,
    signature: sign_secp256k1(&agent_private_key, &tx_hash),
};
```

**Composability via smart contracts:** An Agent can call a smart contract that in turn calls multiple precompiles. Because all calls happen within a single EVM transaction, revm's Journal guarantees atomicity.

```solidity
contract AgentRouter {
    IAgentPrecompile constant AGENT = IAgentPrecompile(0x209);

    function executeStrategy(uint64 agentId, uint64 assetId) external {
        // Bridge to EVM
        AGENT.agentBridgeDeposit(agentId, assetId, address(this), 1000_000000);
        // Then interact with DeFi contracts using EVM-layer tokens
        // ...
    }
}
```

### 6.6 Agent Transaction Signing and Verification

Agents sign standard EVM transactions with their secp256k1 key. The Agent precompile (`0x209`) performs additional verification before executing the operation.

```rust
struct SignedAgentTx {
    evm_tx: EvmTx,                    // standard EVM transaction calling 0x209
    owner_signature: Option<Signature>,  // large transactions require Owner's second confirmation
}
```

**Precompile-level Verification Process:**

```rust
fn verify_agent_tx(tx: &SignedAgentTx) -> Result<()> {
    let agent = get_agent_from_tx(&tx.evm_tx)?;

    // 1. Verify Agent signature (standard EVM recovery)
    let recovered = recover_sender(&tx.evm_tx)?;
    ensure!(recovered == agent.agent_address, "Invalid agent signature");

    // 2. Verify nonce anti-replay
    let current_nonce = get_agent_nonce(agent.owner, agent.agent_id);
    ensure!(tx.evm_tx.nonce == current_nonce, "Invalid nonce");

    // 3. Decode precompile call and verify permissions
    let (precompile_addr, decoded) = decode_precompile_call(&tx.evm_tx.data)?;
    ensure!(precompile_addr == 0x209, "Not an agent precompile call");

    let perms = &agent.permissions;
    if let Some(asset_id) = decoded.asset_id() {
        ensure!(perms.is_allowed(asset_id), "Asset not allowed");
    }
    if let Some(counterparty) = decoded.counterparty() {
        ensure!(perms.is_allowed_counterparty(counterparty), "Counterparty not allowed");
    }
    if let Some(amount) = decoded.amount() {
        if perms.per_tx_limit > 0 {
            ensure!(amount <= perms.per_tx_limit, "Exceeds per-tx limit");
        }
    }

    // 4. Verify Agent has not expired
    if agent.permissions.expires_at > 0 {
        ensure!(current_timestamp() < agent.permissions.expires_at, "Agent expired");
    }

    // 5. Large transactions require Owner's second signature
    let total_amount = decoded.amount().unwrap_or(0);
    if let Some(threshold) = agent.fee_config.require_owner_signature_above {
        if total_amount > threshold {
            ensure!(
                tx.owner_signature.is_some()
                    && tx.owner_signature.unwrap().verify(&agent.owner, &tx.evm_tx.hash()),
                "Owner signature required"
            );
        }
    }

    Ok(())
}
```

### 6.7 Agent Transaction Execution and Fee Handling

Agent transactions are standard EVM transactions that invoke the Agent precompile (`0x209`). They are executed by revm like any other EVM call, with the precompile performing Agent-specific validation and state changes.

```rust
fn execute_agent_tx(tx: &SignedAgentTx) -> Result<()> {
    let agent = get_agent_from_tx(&tx.evm_tx)?;

    verify_agent_tx(tx)?;

    // Gas is handled by standard EVM gas metering; any Agent discount is
    // applied by the precompile returning a reduced gas cost.
    let gas_used = AGENT_PRECOMPILE_GAS; // e.g., 50% of standard rate

    // The precompile deducts Agent sub-account balances and performs the operation
    dispatch_agent_precompile(&tx.evm_tx.data, &agent)?;

    // Update nonce
    increment_agent_nonce(agent.owner, agent.agent_id);

    Ok(())
}
```

### 6.8 Agent Fee Model

Agent payments enjoy exclusive Gas discounts (see §12.2 fee table), with all Agent precompile base fees at 50% of regular users. Agent Gas sponsorship is handled via standard EVM gas fields or sponsor precompiles, typically using `AuthorizedSponsor` mode sponsored by the Owner, so the Agent itself does not need to hold CALL.

### 6.9 Agent Authentication Layers

Agent authentication is organized in three layers:

| Layer | Content | Guarantee |
|------|------|------|
| 1. Cryptographic authentication | Agent key signature + nonce anti-replay | "The Agent itself is operating" |
| 2. Registration attestation | Name + domain verification + code hash | "Who operates this Agent" |
| 3. Trust verification | Behavior history + audit proof + community reputation | "Can I trust it" |

The protocol layer enforces Layer 1, provides infrastructure for Layer 2, and Layer 3 is left to the ecosystem (audit institutions, wallet UI, community).

---

## 7. Issuer Management

### 7.1 Issuer Permissions

Issuers have the following permissions for assets they have registered:

```rust
enum IssuerAction {
    /// Mint (subject to compliance policy constraints)
    Mint { to: Address, amount: u128 },
    /// Burn
    Burn { from: Address, amount: u128 },
    /// Freeze specific address
    FreezeAddress { target: Address },
    /// Unfreeze specific address
    UnfreezeAddress { target: Address },
    /// Update compliance policy
    UpdatePolicy { new_policy: CompliancePolicy },
    /// Transfer issuance authority
    TransferOwnership { new_issuer: Address },
}
```

### 7.2 Restrictions

Issuers **cannot**:
- Modify other issuers' assets
- Bypass protocol-layer compliance policies
- Alter the payment model for protocol payments
- Modify bridge rules
- Directly modify user balances (only through mint/burn)

---

## 8. Network Layer

### 8.1 Protocol

Uses **commonware-p2p** as the P2P network layer, seamlessly integrated with Simplex consensus (commonware-consensus).

- Consensus messages: gossipsub low-latency mode
- Transaction propagation: gossipsub
- Request-response: commonware-p2p request-response mode

### 8.2 Transaction Type Propagation

```
EvmTx (including precompile calls) → gossipsub, standard priority
```

---

## 9. Serialization Format

Callchain uses **unified RLP serialization** -- P2P propagation, block encoding, and storage layer encoding all use the same format, avoiding the complexity of multi-format conversion.

### 9.1 P2P Network and Block Encoding (RLP)

P2P message propagation and block/transaction encoding use **alloy-rlp** (RLP, Recursive Length Prefix), consistent with the Ethereum ecosystem.

```rust
use alloy_rlp::{RlpEncodable, RlpDecodable};

/// P2P network message
struct NetworkMessage {
    data: Vec<u8>,  // RLP-encoded transaction or block
    checksum: u32,
}

impl alloy_rlp::Encodable for EvmTx { ... }
impl alloy_rlp::Decodable for EvmTx { ... }

impl alloy_rlp::Encodable for Block { ... }
impl alloy_rlp::Decodable for Block { ... }
```

**Why RLP over Borsh/SCALE/rkyv:**
| Dimension | RLP | Borsh | SCALE | rkyv |
|------|-----|-------|-------|------|
| Ethereum compatibility | ✅ Native | ❌ Requires conversion | ❌ Requires conversion | ❌ Requires conversion |
| Tool ecosystem | Rich (alloy-rs) | Medium | Polkadot only | Small |
| Storage read/write performance | Native consistency with Reth | Requires adapter layer | Requires adapter layer | Zero-copy read, slow write |
| Reth integration | ✅ Zero adaptation | Requires adapter layer | Requires adapter layer | Requires adapter layer |

### 9.2 JSON-RPC and Configuration (Serde JSON)

RPC interfaces, configuration files, and Genesis use **serde** serialization.

```rust
use serde::{Serialize, Deserialize};

/// Genesis configuration (TOML/JSON parsing)
#[derive(Serialize, Deserialize)]
struct GenesisConfig {
    chain_id: u64,
    initial_validators: Vec<ValidatorInfo>,
    initial_allocations: Vec<Allocation>,
}

/// RPC response
#[derive(Serialize, Deserialize)]
struct RpcResponse<T> {
    jsonrpc: String,
    result: Option<T>,
    error: Option<RpcError>,
    id: u64,
}
```

### 9.3 Storage Layer Encoding

The storage layer (reth-db / MDBX) uses RLP encoding, consistent with the protocol layer:

```rust
/// Storage key-value pairs use RLP encoding
/// Key: fixed-length prefix + variable portion
/// Value: RLP-encoded struct

trait StorageCodec: Sized {
    fn encode_to_buf(&self, buf: &mut Vec<u8>);
    fn decode_from_buf(buf: &[u8]) -> Result<Self>;
}
```

### 9.4 Serialization Strategy by Type

| Data Type | P2P Propagation | Storage | RPC Output |
|----------|---------|------|---------|
| EvmTx | RLP | StorageCodec | JSON |
| SignedAgentTx | RLP | StorageCodec | JSON |
| Block | RLP | StorageCodec | JSON |
| ShieldedProof | RLP | Compressed binary | JSON (base64) |
| StateSnapshot | RLP | StorageCodec | JSON |
| ZkProof | RLP | Compressed binary | JSON (base64) |

### 9.5 Address Format

```
Address = 20 bytes (160 bits), hex encoded, 0x prefix

Example: 0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18

Encoding rules:
  - Internal representation: [u8; 20]
  - Display format: 0x + 40 character hex (lowercase)
  - Checksum: Optional EIP-55 mixed-case checksum
  - Serialization: RLP encodes as 20 raw bytes, JSON encodes as hex string
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
#[repr(transparent)]
pub struct Address([u8; 20]);

impl Address {
    pub fn zero() -> Self { Self([0; 20]) }
    pub fn to_checksum(&self) -> String { /* EIP-55 */ }
}

impl Serialize for Address { /* JSON: hex string */ }
impl<'de> Deserialize<'de> for Address { /* JSON: from hex string */ }
```

### 9.6 Transaction Serialization Example

```rust
// RLP encoding (P2P propagation)
let tx = EvmTx {
    to: Address::from_hex("0x201"),          // transfer precompile
    data: encode_abi("transfer", &[Token::Uint(asset_id.into()), Token::Address(to), Token::Uint(amount.into())]),
    value: 0,
    gas_limit: 21_000,
    max_fee_per_gas: 10_000_000_000,
    max_priority_fee_per_gas: 1_000_000_000,
    nonce: 42,
    chain_id: 1,
    signature: sign_secp256k1(&private_key, &tx_hash),
};
let rlp_bytes = alloy_rlp::encode(&tx);  // → Vec<u8>
let decoded = EvmTx::decode(&mut &rlp_bytes[..])?;

// JSON encoding (RPC output)
let json = serde_json::to_string(&tx)?;
// → {"to":"0x0000000000000000000000000000000000000102","nonce":42,"gasLimit":"0x5208",...}
```

---

## 10. Storage Layer

### 10.1 Engine

Uses **reth-db (MDBX)** as the persistent storage engine.

### 10.2 State Structure

```
callchain/
├── protocol/
│   ├── assets/{asset_id}/          # asset metadata
│   ├── balances/{asset_id}/{addr}  # protocol-layer balances
│   └── allowances/{asset_id}/{owner}/{spender}  # allowances
├── shielded/
│   ├── merkle_tree/{asset_id}/     # Merkle Tree per asset
│   ├── nullifiers/{asset_id}/{nf}  # spent nullifiers (anti-double-spend)
│   ├── commitments/{asset_id}/{cm} # note commitments (encrypted Notes)
│   └── viewing_keys/{addr}/        # user view key mappings
├── agent/
│   ├── registrations/{agent_id}/   # Agent registration info
│   ├── balances/{owner}/{agent_id}/{asset_id}  # Agent sub-account balances
│   └── nonces/{owner}/{agent_id}   # Agent nonce
├── evm/
│   ├── accounts/{addr}/            # EVM accounts
│   ├── contracts/{addr}/           # contract code
│   └── storage/{addr}/{slot}       # contract storage
├── bridge/
│   └── (deprecated — all bridge state lives in EVM storage under 0x103)
├── consensus/
│   ├── blocks/{height}             # block data
│   └── state/{height}              # state snapshots
└── metadata/
    ├── chain_id                    # chain ID
    ├── validators                  # current validator set
    ├── compliance                  # compliance policy registry
    └── agents/                     # Agent registry
        └── {agent_id}/             # Agent identity and permissions
```

### 10.3 Data Prune Strategy

Blockchain node data grows infinitely over time. Callchain adopts a **layered prune strategy**: retain current state + most recent N blocks, historical intermediate state can be safely discarded.

#### 10.3.1 Data Classification

```
Node data = Current state + Historical blocks + Intermediate state + Indexes + Archive data

Must retain (cannot prune):
  ✓ Current protocol-layer balances
  ✓ Current EVM state
  ✓ Current Shielded Pool state (nullifier set, Merkle root)
  ✓ Current Agent registrations and balances
  ✓ Block headers (for chain verification)
  ✓ Complete data of the most recent N blocks

Can be pruned:
  ✗ Historical intermediate state versions
  ✗ Execution traces of confirmed transactions
  ✗ Expired transaction indexes (> N blocks ago)
  ✗ Archived contract intermediate storage
```

#### 10.3.2 Layered Prune Configuration

```rust
struct PruneConfig {
    // Snapshot: generate a full state snapshot every E blocks
    snapshot_interval: u64,        // default 100_000 blocks (~7 hours)
    snapshot_keep: u64,            // default 3 (keep most recent 3 snapshots)

    // Prune: trigger prune every P blocks
    prune_interval: u64,           // default 10_000 blocks

    // Hot data: retain most recent complete state
    keep_recent: u64,              // default 50_000 blocks full state

    // Block headers: always retained (for light client verification)
    // Block bodies: after keep_recent, only headers retained, transaction details discarded
    keep_block_body: u64,          // default 100_000 blocks

    // Receipts/logs: retained longer for block explorer queries
    keep_receipt: u64,             // default 1_000_000 blocks

    // Node mode
    node_mode: NodeMode,
}

enum NodeMode {
    /// Validator: retain full state + most recent 100K blocks
    Validator,
    /// Full node: prune historical intermediate state, retain current state
    Full,
    /// Light node: only retain block headers, state queried on demand
    Light,
    /// Archive node: retain all historical data (run by community/analysts)
    Archive,
}
```

#### 10.3.3 Prune Rules by Layer

**Protocol Layer:**
```
- Current balances: always retained (incremental updates, overwrite old values)
- Allowances: always retained
- Asset registry: always retained
- Historical transaction traces: pruned after keep_recent
```

**EVM Layer:**
```
- Current EVM state: always retained
- Contract code: always retained
- Historical storage versions: pruned after keep_recent
- Transaction traces: discarded per prune policy
```

**Shielded Pool:**
```
Nullifier set (cannot be pruned):
  - Must be fully retained, otherwise double-spending cannot be detected
  - Compressed storage using BitSet, ~1 bit / spent nullifier
  - Estimated 100M transactions ~ 12.5 MB

Merkle Tree (partial prune):
  - Root hash: always retained
  - Branch nodes: retained to depth allowing fast verification
  - Encrypted Note data: can be pruned to archive nodes after keep_recent
  - Viewing key mappings: always retained

Commitment tree growth estimate:
  Each Shielded transaction ~2 commitments
  100K/day × 365 days = 36.5M commitments
  Each commitment ~32 bytes = ~1.2 GB/year (acceptable)
```

**Agent Layer:**
```
- Current registration info: always retained
- Current balances: always retained
- Current nonce: always retained
- Historical transactions: pruned after keep_recent
```

**Consensus Layer:**
```
- Block headers: always retained
- Block bodies (transaction details): pruned after keep_block_body
- State snapshots: retain most recent snapshot_keep
- Historical validator sets: pruned after keep_recent
```

#### 10.3.4 State Snapshot Generation and Verification

```rust
struct StateSnapshot {
    height: u64,
    state_root: Hash,            // EVM state root (single source of truth)
    shielded_root: Hash,         // Shielded Merkle root (read from EVM)
    agent_root: Hash,            // Agent state root (computed from EVM)
    consensus_root: Hash,        // validator set hash (read from EVM)
    total_size: u64,             // snapshot size (bytes)
    validator_signatures: Vec<ValidatorSignature>,  // 2/3 signatures
}

// Snapshot generation (every snapshot_interval blocks)
// All roots are derived from EVM storage; no separate protocol-state trees.
fn generate_snapshot(state: &State, height: u64) -> StateSnapshot {
    let evm_root = compute_evm_root(&state.evm);
    StateSnapshot {
        height,
        state_root: evm_root,
        shielded_root: read_shielded_merkle_root(&state.evm),
        agent_root: compute_agent_root_from_evm(&state.evm),
        consensus_root: compute_consensus_root_from_evm(&state.evm),
        total_size: estimate_state_size(state),
        validator_signatures: collect_validator_signatures(state),
    }
}

// Snapshot verification
fn verify_snapshot(snapshot: &StateSnapshot) -> Result<()> {
    // Requires 2/3 validator signatures
    let valid_sigs = snapshot.validator_signatures
        .iter()
        .filter(|sig| verify_validator_sig(sig, &snapshot))
        .count();
    ensure!(valid_sigs >= quorum(), "Insufficient validator signatures");
    Ok(())
}
```

#### 10.3.5 Fast Sync Process

New nodes use snapshot + incremental sync, no need to replay from genesis block:

```
1. Fetch recent state snapshot from network (from other nodes or P2P snapshot market)
2. Verify snapshot:
   - Check 2/3 validator signatures
   - Verify Merkle root consistency
3. Restore snapshot state to local storage
4. Sync subsequent data block by block from snapshot height
5. After reaching latest height, begin participating in consensus/verification

Estimated time:
  - Snapshot download: ~30 seconds (100MB @ 100Mbps)
  - Snapshot verification: ~5 seconds
  - State restore: ~30 seconds
  - Incremental sync: ~2-3 minutes (depending on number of blocks behind)
  - Total: < 5 minutes
```

#### 10.3.6 Prune Trigger and Cleanup

```rust
// Periodic prune check
fn maybe_prune(state: &State, config: &PruneConfig) -> Result<()> {
    let current_height = state.current_height();

    if current_height % config.prune_interval != 0 {
        return Ok(());
    }

    let prune_boundary = current_height.saturating_sub(config.keep_recent);

    // 1. Prune historical transaction traces
    prune_execution_traces(prune_boundary)?;

    // 2. Prune expired receipts/logs
    prune_receipts(current_height.saturating_sub(config.keep_receipt))?;

    // 3. Prune expired block bodies
    prune_block_bodies(current_height.saturating_sub(config.keep_block_body))?;

    // 4. Clean up expired snapshots (retain most recent N)
    prune_old_snapshots(config.snapshot_keep)?;

    // 5. Compact database (release physical disk space)
    compact_database()?;

    Ok(())
}
```

#### 10.3.7 Storage Growth Estimate

| Node Mode | Annual Growth | Total Size After 1 Year | Description |
|----------|---------|------------|------|
| Validator | ~2 GB/month | ~50 GB | Hot data + most recent 50K blocks |
| Full | ~500 MB/month | ~20 GB | Current state + block headers |
| Light | ~100 MB/month | ~5 GB | Block headers only |
| Archive | ~50 GB/month | ~600 GB | Retains all history |

#### 10.3.8 CLI Configuration Example

```bash
# Validator node (default)
calld run --mode validator

# Full node (prune history, retain current state)
calld run --mode full \
  --prune.keep-recent 50000 \
  --prune.keep-receipt 1000000 \
  --prune.keep-block-body 100000

# Light node (block headers only, suitable for embedded devices)
calld run --mode light

# Archive node (retain all history, for analysis/explorer)
calld run --mode archive

# Custom snapshot strategy
calld run \
  --snapshot.interval 100000 \
  --snapshot.keep 3
```

---

## 11. RPC Interface

### 11.1 Standard Ethereum JSON-RPC

Fully supports the Ethereum JSON-RPC 2.0 specification:
- `eth_call`, `eth_sendRawTransaction`
- `eth_getBalance`, `eth_getTransactionReceipt`
- `eth_blockNumber`, `eth_getLogs`
- ... all standard methods

### 11.2 Callchain Extension Methods

```json
// Protocol-layer asset query
{
    "method": "call_assetInfo",
    "params": [42],
    "id": 1
}
→ { asset_id, name, symbol, decimals, issuer, total_supply, policy }

// Protocol-layer balance query
{
    "method": "call_protocolBalance",
    "params": [42, "0x..."],
    "id": 1
}
→ { balance: "1000000000000000000" }

// Submit protocol payment transaction
{
    "method": "call_sendPayment",
    "params": [{ asset_id, from, to, amount, signature }],
    "id": 1
}
→ { tx_hash }

// Asset registration
{
    "method": "call_register",
    "params": [{ name, symbol, decimals, policy, signature }],
    "id": 1
}
→ { asset_id, evm_contract }

// Compliance policy query
{
    "method": "call_compliancePolicy",
    "params": [42],
    "id": 1
}
→ { policy: "OfacBlacklist", details: {...} }

// Unified balance query
{
    "method": "call_totalBalance",
    "params": [42, "0x..."],
    "id": 1
}
→ { protocol: "30", evm: "70", total: "100" }

// Agent registration
{
    "method": "call_agentRegister",
    "params": [{ name, agent_public_key, url, permissions, fee_config, signature }],
    "id": 1
}
→ { agent_id: 42 }

// Agent info query
{
    "method": "call_agentInfo",
    "params": [42],
    "id": 1
}
→ { agent_id, owner, name, url, permissions, fee_config, registered_at }

// Agent balance query
{
    "method": "call_agentBalance",
    "params": ["0x...", 42, 1],
    "id": 1
}
→ { balance: "500000000", asset_id: 1 }

// Agent transaction history
{
    "method": "call_agentHistory",
    "params": [42, { from_block: 0, to_block: "latest" }],
    "id": 1
}
→ { transactions: [...], total_spent: "...", days_active: 365 }

// Agent fund authorization
{
    "method": "call_agentGrant",
    "params": [{ agent_id, amount, asset_id, signature }],
    "id": 1
}
→ { tx_hash }

// Agent revocation
{
    "method": "call_agentRevoke",
    "params": [{ agent_id, signature }],
    "id": 1
}
→ { tx_hash }

// Shielded Pool: generate deposit proof (client-side, off-chain)
{
    "method": "call_shieldedDepositProve",
    "params": [{ asset_id, amount, recipient }],
    "id": 1
}
→ { commitment, encrypted_note }

// Shielded Pool: generate shielded transfer proof (client-side, off-chain)
{
    "method": "call_shieldedTransferProve",
    "params": [{ asset_id, notes_to_spend, recipients, viewing_key }],
    "id": 1
}
→ { nullifiers, commitments, proof }

// Shielded Pool: balance query (requires viewing key)
{
    "method": "call_shieldedBalance",
    "params": [{ asset_id, viewing_key }],
    "id": 1
}
→ { total: "1000", notes: [{ commitment, encrypted_amount }] }

// Shielded Pool: Merkle Tree state
{
    "method": "call_shieldedTreeState",
    "params": [42],
    "id": 1
}
→ { depth: 32, leaf_count: 15234, root: "0x..." }
```

### 11.3 WebSocket

Supports WebSocket subscriptions:
- `call_newPaymentBlock` -- new block
- `call_paymentReceived` -- received protocol payment
- `call_bridgeCompleted` -- bridge completed
- `call_assetRegistered` -- new asset registered
- `call_agentExecuted` -- Agent transaction executed
- `call_agentRevoked` -- Agent revoked
- `call_shieldedDeposit` -- received Shielded deposit (requires viewing key)
- `call_shieldedWithdrawal` -- Shielded withdrawal to transparent address

---

## 12. Economic Model

### 12.1 CALL Token

CALL is Callchain's native token, serving the triple role of Gas, staking, and governance.

| Attribute | Value |
|------|------|
| Total Supply | 1,000,000,000 CALL (1 billion, fixed) |
| Smallest Unit | 1 wei = 10⁻¹⁸ CALL |
| Issuance | None, fixed supply |
| Deflationary Mechanism | 50% transaction fee burn |

**Dual-Balance Model:**

CALL operates under an independent dual-balance architecture:
- **EVM native balance**: Used for gas payment and native EVM transfers. Genesis allocates CALL as EVM balance only.
- **Protocol balance**: Tracked in `AssetStorage` (asset_id = 1). Used for protocol-layer transfers via the Asset precompile (`0x201`).

The two balances are **never automatically synchronized**. A user's total CALL is the sum of both layers. The Switch precompile (`0x207`) provides `switchToEvm` and `switchToProtocol` as the manual migration path between layers.

**Initial Allocation:**

| Category | Proportion | Amount | Lock-up |
|------|------|------|------|
| Validator rewards | 70% | 350M | Linear release over 8 years |
| Ecosystem fund | 20% | 100M | Multi-sig managed, community governance |
| Community airdrop | 10% | 50M | 30% released at mainnet launch, remaining linear over 24 months |
| Historically allocated | - | 500M | Completed pre-genesis allocation |

### 12.2 Gas Payment (EIP-1559 Dynamic Fee)

All protocol-layer transactions pay Gas in CALL. Uses an **EIP-1559-like dynamic fee**: each precompile call has a fixed gas cost, and the network base rate adjusts automatically each block, rising during congestion and falling during idle periods.

#### 12.2.1 Precompile Gas Cost Table

| Precompile Operation | Gas Cost | Description |
|----------|---------|------|
| Transfer | 10,000 gas | Standard transfer |
| Transfer (with Memo) | 10,000 + memo_bytes × 1 gas | Transfer with memo |
| Approve / Mint / Burn | 5,000 gas | Approve/mint/burn |
| Each recipient in BatchTransfer | 1,000 gas | Per payee in batch payment |
| BatchTransfer Memo | memo_bytes × 1 gas | Batch memo additional fee |
| SwitchToEvm / SwitchToProtocol | 20,000 gas (+ nested EVM call gas) | Internal bridge via Switch precompile (0x207) |
| ShieldedDeposit / Withdraw | 20,000 gas | Includes ZK proof verification |
| ShieldedTransfer | 50,000 gas | Includes ZK proof verification |
| Agent precompiles | Above × 0.5 | Agent exclusive discount |
| ExternalBridgeDeposit | 30,000 gas | External bridge (includes signature verification) |

#### 12.2.2 Fee Calculation Formula

```
Total fee = base_fee × total gas units + priority_fee

Where:
  base_fee: dynamically adjusted base rate per block (wei/gas unit)
  total_gas: sum of gas consumed by all precompile calls in the transaction
  priority_fee: user-selected priority tip (all goes to validators)
```

#### 12.2.3 Base Fee Dynamic Adjustment

```rust
/// Per-block base fee adjustment parameters
struct FeeParams {
    base_fee: u128,               // current base rate (wei/gas)
    target_gas_per_block: u64,   // target gas usage (block)
    max_gas_per_block: u64,      // maximum gas limit
    adjustment_coefficient: u128, // adjustment coefficient (1/8 = 12.5%)
}

/// Update base fee each block
fn update_base_fee(current_base_fee: u128, block_gas_used: u64, params: &FeeParams) -> u128 {
    let target = params.target_gas_per_block;
    if block_gas_used == target {
        return current_base_fee;  // exactly at target, no change
    }

    let adjustment = current_base_fee
        * (block_gas_used as i128 - target as i128).abs() as u128
        / target as u128
        / 8;  // maximum adjustment 12.5%

    if block_gas_used > target {
        // Congested: price increase, max +12.5%
        current_base_fee.saturating_add(adjustment)
    } else {
        // Idle: price decrease, max -12.5%
        current_base_fee.saturating_sub(adjustment)
    }
}
```

**Base Fee Adjustment Example (assuming initial base_fee = 1 wei/gas, target_gas = 10M):**

| Scenario | Block Gas Usage | base_fee Change |
|------|-------------|--------------|
| Exactly at target | 10,000,000 | No change |
| Light congestion | 12,000,000 (+20%) | +2.5% |
| Severe congestion | 15,000,000 (+50%) | +6.25% |
| Full load | 20,000,000 (+100%) | +12.5% |
| Idle | 5,000,000 (-50%) | -6.25% |
| Extremely idle | 0 | -12.5% |

**During continuous congestion, base_fee grows exponentially:** +12.5% per block, base_fee doubles after 6 consecutive full-load blocks.

#### 12.2.4 Fee Calculation Example

```rust
// Example: smart contract calling two precompiles (Transfer + Approve)
// Parameters: base_fee = 10 wei/gas, priority_fee = 50,000 wei

let gas_units = vec![10_000, 5_000];  // gas for two precompile calls
let total_gas: u64 = gas_units.iter().sum();
// = 10,000 + 5,000 = 15,000 gas

let total_fee = base_fee * total_gas + priority_fee;
// = 10 × 15,000 + 50,000 = 200,000 wei = 0.0000002 CALL
```

#### 12.2.5 Fee Distribution

**When paying with CALL:**
```
Total fee paid by user = base_fee × gas + priority_fee (in CALL)
  ├── base_fee × gas × 50% → validator rewards (CALL)
  ├── base_fee × gas × 50% → directly burned (deflationary, CALL only)
  └── priority_fee 100%    → validator that packed the transaction (CALL)
```

**When paying with stablecoin:**
```
Total fee paid by user = base_fee × gas + priority_fee (in stablecoin)
  ├── base_fee × gas × 50% → validator rewards (stablecoin)
  ├── base_fee × gas × 50% → into treasury reserve (stablecoin, not burned)
  └── priority_fee 100%    → validator that packed the transaction (stablecoin)
```

**Multi-currency summary (per block):**
```
Block revenue summary:
  CALL:    1000 CALL → 50% burned + 50% to validators
  USDC:    20 USDC   → 50% treasury reserve + 50% to validators
  USDT:    5 USDT    → 50% treasury reserve + 50% to validators

Validator A (weight 1/8) receives:
  CALL:  1000 × 50% × 1/8 = 62.5 CALL (reward)
  USDC:  20 × 50% × 1/8 = 1.25 USDC (reward)
  USDT:  5 × 50% × 1/8 = 0.3125 USDT (reward)
  + priority_fee (all to packing validator)
```

#### 12.2.6 Configuration Parameters

| Parameter | Default Value | Description |
|------|--------|------|
| initial_base_fee | 10 wei/gas | Initial base rate |
| target_gas_per_block | 10,000,000 gas | Target block gas usage |
| max_gas_per_block | 20,000,000 gas | Maximum block gas limit (2× target) |
| adjustment_coefficient | 1/8 | Maximum adjustment 12.5% per block |
| min_base_fee | 1 wei/gas | base_fee floor (prevents zero) |
| max_base_fee | 1,000,000,000 wei/gas | base_fee ceiling (prevents extreme cases) |

#### 12.2.7 MemPool Admission

```rust
fn accept_to_mempool(tx: &EvmTx) -> Result<()> {
    let current_base_fee = get_current_base_fee();
    let estimated_gas = estimate_evm_gas(tx);

    // 1. gas_limit check
    ensure!(tx.gas_limit >= estimated_gas, "Gas limit too low");

    // 2. Fee check: max_fee_per_gas must cover current base_fee
    let min_max_fee = current_base_fee + tx.max_priority_fee_per_gas;
    ensure!(tx.max_fee_per_gas >= min_max_fee,
            "max_fee_per_gas insufficient for current base_fee");

    // 3. Balance sufficient to cover maximum possible fee + value
    let sender = recover_sender(tx)?;
    let sender_balance = get_evm_balance(sender);
    let max_fee_cost = tx.max_fee_per_gas * tx.gas_limit as u128;
    ensure!(sender_balance >= max_fee_cost + tx.value,
            "Insufficient balance");

    Ok(())
}
```

### 12.3 Gas Sponsorship Mechanism

The protocol layer natively supports Gas sponsorship without requiring smart contracts. Sponsors do not need to sign every transaction (except in per-transaction sponsorship mode).

#### 12.3.0 Stablecoin Direct Gas Payment

The protocol allows users to pay Gas fees directly with governance-approved stablecoins. Validators receive rewards in the actual stablecoin received, with no currency conversion performed by the protocol.

**Stablecoin Registry:**

```rust
/// Allowed Gas payment currencies
struct FeeCurrencyEntry {
    asset_id: AssetId,
    name: String,                // e.g., "USDC"
    decimals: u8,
    oracle_price_key: String,    // oracle price pair, e.g., "USDC_CALL"
    added_at_block: u64,
    added_by_proposal: u64,      // governance proposal ID
}

struct FeeCurrencyRegistry {
    /// List of approved stablecoins
    allowed_currencies: Vec<FeeCurrencyEntry>,
    /// Per-block stablecoin Gas payment cap (proportion of total fees, 0-10000 bps)
    stablecoin_cap_bps: u16,     // default 5000 = 50%
}

impl FeeCurrencyRegistry {
    /// Verify stablecoin is in governance-approved list
    fn is_allowed(asset_id: AssetId) -> bool {
        Self::allowed_currencies.iter().any(|e| e.asset_id == asset_id)
    }

    /// Query oracle price for specified stablecoin (asset → CALL)
    fn get_call_price(asset_id: AssetId) -> Option<u128> {
        let entry = Self::allowed_currencies.iter()
            .find(|e| e.asset_id == asset_id)?;
        OracleSystem::get_median_price(&entry.oracle_price_key)
    }
}
```

**Price Conversion Logic:**

```rust
/// Convert CALL-denominated fee to target stablecoin amount
fn convert_fee_to_stablecoin(asset_id: AssetId, fee_call: u128) -> Result<u128> {
    // oracle price: 1 unit stablecoin = price CALL
    // e.g., 1 USDC = 50 CALL → price = 50_000_000_000_000_000_000 (18 decimals)
    let price_call = FeeCurrencyRegistry::get_call_price(asset_id)
        .ok_or("No oracle price for fee currency")?;

    // stablecoin_amount = fee_call / price_call × 10^decimals
    let decimals = get_asset_decimals(asset_id);
    let fee_stablecoin = fee_call
        .checked_mul(10u128.pow(decimals as u32))
        .ok_or("Overflow in fee conversion")?
        .checked_div(price_call)
        .ok_or("Division by zero in fee conversion")?;

    // Round up to ensure protocol does not receive less
    Ok(fee_stablecoin)
}
```

**Execution Flow:**

```
1. User submits transaction: fee_currency = Stablecoin(USDC)
2. Protocol verifies: USDC is in FeeCurrencyRegistry ✓
3. Query oracle: 1 USDC = 50 CALL
4. Calculate: fee_call = gas_used × base_fee
5. Convert: fee_usdc = fee_call / 50 (rounded up)
6. Deduct USDC balance from sender
7. USDC counted in block fee summary
8. Validators receive USDC rewards proportionally
```

**Governance Proposal -- Add/Remove Stablecoin:**

```rust
// Governance proposal type extension
enum ProposalType {
    // ... existing types
    FeeCurrencyAdd {
        asset_id: AssetId,
        name: String,
        oracle_price_key: String,
    },
    FeeCurrencyRemove {
        asset_id: AssetId,
        grace_period_blocks: u64,  // grace period before removal
    },
    FeeCurrencyCap {
        new_cap_bps: u16,          // new stablecoin payment cap (bps)
    },
}
```

| Parameter | Default Value | Description |
|------|--------|------|
| stablecoin_cap_bps | 5000 (50%) | Per-block stablecoin Gas cap as percentage of total fees |
| min_market_cap_usd | 100,000,000 | Governance admission minimum market cap |
| oracle_strikes_before_disable | 10 | Auto-disable after oracle anomaly strikes |
| grace_period_blocks | 86,400 (~1 day) | Grace period before removal |

**Mempool Priority Sorting:**

```rust
fn priority_score(tx: &EvmTx) -> u128 {
    // Standard EIP-1559 priority: max_priority_fee_per_gas
    tx.max_priority_fee_per_gas
}
```

**Validator Fee Distribution (Multi-Currency):**

```
Block fee summary:
  CALL revenue:    1000 CALL
  USDC revenue:    20 USDC
  USDT revenue:    5 USDT

Distribution:
  50% base_fee × 50% burn → only CALL portion burned
  50% base_fee × 50% validators → distributed proportionally by currency
  priority_fee 100% → packing validator

Validator A receives: 250 CALL + 5 USDC + 1.25 USDT + priority_fee
```
```

| Mode | Applicable Scenario | Sponsor Needs to Be Online | Control Granularity |
|------|---------|------------|---------|
| SelfPay | Regular users | - | - |
| AuthorizedSponsor | Agent payments, platform subsidies | Not required | Per-daily-limit + whitelist |
| PoolSponsor | Platform batch user subsidies | Not required | Total deposit control |
| PerTxSponsor | Low-frequency single sponsorship | Required | Per-transaction control |

#### 12.3.2 Pre-Authorized Sponsorship (AuthorizedSponsor)

Sponsors sign one authorization, allowing specified addresses to use their CALL for Gas payment.

```rust
struct GasSponsorAuth {
    sponsor: Address,           // sponsor
    allowed_senders: Vec<Address>,  // addresses that can be sponsored (empty = anyone)
    max_daily: u128,            // daily Gas limit (0 = unlimited)
    expires_at: u64,            // expiry timestamp (0 = never expires)
    sponsor_signature: Signature,  // sponsor signature
}
```

**Authorization Registration:**

```rust
fn register_sponsor_auth(auth: GasSponsorAuth) -> Result<()> {
    verify_signature(&auth.sponsor, &auth.sponsor_signature, &auth.hash())?;
    GasSponsorAuths::insert(auth.sponsor, auth);
}

// Sponsors can revoke at any time
fn revoke_sponsor_auth(sponsor: Address) -> Result<()> {
    GasSponsorAuths::remove(sponsor);
}
```

**Runtime Sponsor Verification:**

```rust
fn verify_and_deduct_authorized_sponsor(
    sponsor: Address,
    sender: Address,
    fee: u128,
) -> Result<()> {
    let auth = GasSponsorAuths::get(sponsor).ok_or("No sponsor auth")?;

    // 1. Verify not expired
    if auth.expires_at > 0 {
        ensure!(current_timestamp() < auth.expires_at, "Sponsor auth expired");
    }

    // 2. Verify sender is in whitelist
    if !auth.allowed_senders.is_empty() {
        ensure!(auth.allowed_senders.contains(&sender), "Sender not whitelisted");
    }

    // 3. Verify daily limit
    if auth.max_daily > 0 {
        let today = current_timestamp() / 86400;
        let (last_date, spent) = GasSponsorDailyUsage::get(sponsor).unwrap_or((0, 0));
        if today != last_date {
            GasSponsorDailyUsage::insert(sponsor, (today, 0));
        } else {
            ensure!(spent + fee <= auth.max_daily, "Daily limit exceeded");
            GasSponsorDailyUsage::insert(sponsor, (today, spent + fee));
        }
    }

    // 4. Deduct fee
    deduct_call_balance(sponsor, fee)?;
    Ok(())
}
```

#### 12.3.3 Pre-Deposit Pool Sponsorship (PoolSponsor)

Sponsors pre-deposit a sum of CALL in the protocol layer, authorizing specific addresses to use it, with the system automatically deducting fees from the pool.

```rust
struct GasSponsorPool {
    sponsor: Address,
    balance: u128,                   // pre-deposited balance
    delegated_to: Vec<Address>,      // authorized addresses (empty = anyone)
    per_tx_limit: u128,              // per-transaction limit (0 = unlimited)
}

fn deposit_to_pool(sponsor: Address, amount: u128) -> Result<()> {
    deduct_call_balance(sponsor, amount)?;
    let pool = GasSponsorPools::entry(sponsor).or_default();
    pool.balance += amount;
}

fn withdraw_from_pool(sponsor: Address, amount: u128) -> Result<()> {
    let pool = GasSponsorPools::get(sponsor).ok_or("No pool")?;
    ensure!(pool.balance >= amount, "Insufficient pool balance");
    pool.balance -= amount;
    credit_call_balance(sponsor, amount)?;
}
```

**Use Case:** Platform subsidizes Gas for new users; when users submit transactions, they use a sponsor precompile or ERC-4337 paymaster, and the system automatically deducts CALL from the platform pool.

#### 12.3.4 Per-Transaction Sponsorship (PerTxSponsor)

Sponsors sign confirmation for each transaction, suitable for low-frequency scenarios.

```rust
// Transaction structure includes sponsor signature via sponsor precompile or paymaster
// The sponsor signs an authorization that is included in the transaction data

fn verify_and_deduct_per_tx_sponsor(
    sponsor: Address,
    sponsor_signature: Signature,
    tx_hash: Hash,
    fee: u128,
) -> Result<()> {
    verify_signature(&sponsor, &sponsor_signature, tx_hash)?;
    deduct_call_balance(sponsor, fee)?;
}
```

### 12.4 Fee Distribution

```
User pays CALL:
  ├── 50% → Validator rewards (distributed by stake proportion)
  ├── 50% → Directly burned (deflationary mechanism)
```

### 12.5 No Inflation

- No token issuance
- Validator income 100% from transaction fees
- 50% fee burn makes CALL total supply continuously deflationary
- Long-term validator income driven by on-chain economic activity

### 12.6 Validator Staking

```rust
struct ValidatorStake {
    validator_id: ValidatorId,
    staked_call: u128,              // staked CALL amount
    self_stake: u128,               // self-staked portion
    delegated_call: u128,           // delegated stake
    rewards: u128,                  // unclaimed rewards
    slash_history: Vec<SlashEvent>, // slashing history
}
```

| Parameter | Value |
|------|------|
| Minimum self-stake | 1,000,000 CALL |
| Delegate stake limit | No limit |
| Unlock period | 7 days (~2,419,200 blocks) |
| Double-sign slashing | Deduct all self-stake |
| Offline penalty | Proportional deduction based on offline rounds |

### 12.7 Fee AMM (Optional Upgrade Path)

Future support for users to pay Gas with non-CALL assets, through an internal AMM for automatic conversion:

```
User pays USDC → AMM automatically buys CALL → 50% to validators / 50% burned
```

Currently only CALL payment for Gas is supported.

---

## 13. Security Design

### 13.1 Cryptography

| Purpose | Algorithm |
|------|------|
| Transaction signing | secp256k1 |
| Consensus signing | ed25519 |
| State commitment | SHA-256 + Merkle Tree |
| Address generation | Keccak-256 (EVM compatible) |

### 13.2 MEV Protection

- PBS (Proposer-Builder Separation) built-in
- Validators do not participate in MEV extraction
- Payment transactions encrypted in mempool (commit-reveal)

### 13.3 On-Chain Governance

Callchain adopts **dual-track governance**: validators vote on technical parameters, CALL holders vote on ecosystem decisions. Both are coordinated through a timelock for execution.

#### 13.3.1 Governance Architecture

```rust
/// Governance proposal
struct Proposal {
    id: u64,
    proposer: Address,              // proposer
    proposal_type: ProposalType,     // proposal type
    title: String,                   // title
    description: String,             // detailed description
    voting_power_yes: u128,          // yes vote weight
    voting_power_no: u128,           // no vote weight
    voting_power_abstain: u128,      // abstain vote weight
    start_block: u64,                // voting start block
    end_block: u64,                  // voting end block
    execution_block: u64,            // timelock execution block
    state: ProposalState,
    quorum_required: u128,           // quorum threshold
    execution_data: Vec<u8>,         // serialized execution data
}

enum ProposalType {
    /// Technical parameter changes (gas price, block size, validator count, etc.)
    ParameterChange { param_id: u64, new_value: Vec<u8> },
    /// Protocol upgrade (new features, precompile operations, ZK circuits, etc.)
    ProtocolUpgrade { activation_block: u64, changelog: String },
    /// Ecosystem fund allocation (community project funding, airdrops, etc.)
    TreasurySpend { recipient: Address, amount: u128, asset_id: AssetId },
    /// Validator penalty proposal (slash malicious validators)
    ValidatorSlash { validator_id: ValidatorId, reason: String },
    /// Compliance policy update (OFAC blacklist update, etc.)
    ComplianceUpdate { asset_id: AssetId, new_policy: CompliancePolicy },
    /// Emergency pause (consensus layer bug, requires 2/3 validator joint signature)
    EmergencyPause { reason: String },
}

enum ProposalState {
    Pending,        // waiting for voting to start
    Active,         // voting in progress
    Passed,         // voting passed, waiting for timelock
    Defeated,       // voting failed
    Queued,         // entered timelock queue
    Executed,       // executed
    Expired,        // timelock expired without execution
}
```

#### 13.3.2 Dual-Track Voting

| Decision Type | Voting Body | Passing Threshold | Timelock |
|---------|---------|---------|--------|
| Technical parameter change | Validators (1 validator = 1 vote) | 2/3 majority | 7 days |
| Protocol upgrade | Validators + CALL holders | 2/3 validators + >50% CALL | 14 days |
| Ecosystem fund allocation | CALL holders (1 CALL = 1 vote) | >50% CALL voting + 60% yes | 7 days |
| Validator penalty | Validators | 2/3 majority | Immediate |
| Emergency pause | Validators | 2/3 majority | Immediate |

```rust
/// Voting power calculation
fn calculate_voting_power(proposal: &Proposal, voter: Address) -> u128 {
    match proposal.proposal_type {
        ProposalType::ParameterChange { .. }
        | ProposalType::ProtocolUpgrade { .. }
        | ProposalType::ValidatorSlash { .. }
        | ProposalType::EmergencyPause { .. } => {
            // Validator voting: 1 validator = 1 vote
            if is_validator(voter) { 1 } else { 0 }
        }
        ProposalType::TreasurySpend { .. } => {
            // Community voting: CALL balance weighted
            get_call_balance(voter)
        }
        ProposalType::ComplianceUpdate { .. } => {
            // Asset issuer + validator joint voting
            let asset = get_asset(proposal.proposal_type.asset_id());
            if asset.issuer == voter { asset.total_supply / 10 } // issuer 10% weight
            else if is_validator(voter) { 1 }
            else { 0 }
        }
    }
}

/// Vote delegation
/// CALL holders can delegate voting power to third parties
struct VoteDelegation {
    delegator: Address,
    delegate: Address,
    amount: u128,               // delegated CALL amount
    expires_at: u64,            // expiry (0=permanent)
}

type VoteDelegations = HashMap<Address, Vec<VoteDelegation>>; // delegate → delegations

fn get_delegated_voting_power(delegate: Address, asset_id: AssetId) -> u128 {
    VoteDelegations::get(delegate)
        .iter()
        .filter(|d| d.expires_at == 0 || current_timestamp() < d.expires_at)
        .map(|d| d.amount)
        .sum()
}
```

#### 13.3.3 Proposal Process

```
1. Submit proposal
   - Deposit (anti-spam, 10,000 CALL)
   - Specify proposal type, parameters, execution data
   - Proposal enters 2-day review period

2. Voting period (7 days)
   - Validators/CALL holders vote by weight
   - Options: Yes / No / Abstain
   - Votes counted on-chain in real time

3. Result determination
   - Quorum reached and yes votes > no votes → Passed
   - Quorum not reached or no votes > yes votes → Defeated

4. Timelock (7-14 days, depending on proposal type)
   - Passed proposals enter timelock queue
   - Community has time to coordinate upgrades or exit
   - Anyone can trigger execution

5. Execution
   - Automatically executed after reaching execution_block
   - Execution data written to state
   - Deposit refunded to proposer

6. Timeout
   - Not executed within 30 days after execution_block → Expired
   - Deposit confiscated, added to ecosystem fund
```

#### 13.3.4 Quorum

```rust
fn calculate_quorum(proposal_type: &ProposalType) -> u128 {
    match proposal_type {
        ProposalType::ParameterChange { .. } => {
            // 2/3 validators participate
            validator_count() * 2 / 3
        }
        ProposalType::ProtocolUpgrade { .. } => {
            // 2/3 validators + 20% of total CALL supply participate
            max(validator_count() * 2 / 3, total_call_supply() / 5)
        }
        ProposalType::TreasurySpend { .. } => {
            // 20% of total CALL supply participates
            total_call_supply() / 5
        }
        _ => validator_count() / 2 + 1,  // simple majority
    }
}
```

#### 13.3.5 Governance Parameters

| Parameter | Value |
|------|------|
| Proposal deposit | 10,000 CALL |
| Review period | 2 days (~691,200 blocks) |
| Voting period | 7 days (~2,419,200 blocks) |
| Timelock (parameter change) | 7 days |
| Timelock (protocol upgrade) | 14 days |
| Timelock (emergency) | Immediate |
| Execution timeout | 30 days |
| Vote delegation | Supported, revocable |
| Quadratic voting | Not supported (1 CALL = 1 vote) |

### 13.4 Upgrade Mechanism

- Triggered via governance proposal (see §19)
- Emergency pause only for consensus-layer bugs (requires 2/3 validator signatures)
- No multi-sig management contract

### 13.5 Network Attack Protection

#### 13.5.1 Block-Level Limits

```rust
/// Block configuration limits
struct BlockLimits {
    /// Maximum block size in bytes (RLP encoded)
    max_block_size: u64,             // default 5 MB

    /// Maximum transactions per block
    max_transactions: u32,           // default 10,000

    /// Shielded transaction cap (ZK proof verification is costly)
    max_shielded_per_block: u32,     // default 50

    /// Maximum precompile calls per transaction
    max_precompile_calls_per_tx: u32,    // default 1,000

    /// Maximum transaction size in bytes
    max_tx_size: u32,                // default 256 KB

    /// Maximum recipients in batch transfer
    max_batch_payments: u32,         // default 5,000

    /// EVM block gas limit
    max_evm_gas_per_block: u64,      // default 30,000,000
}
```

Validators enforce these limits when packing blocks; excess is queued for the next block.

#### 13.5.2 Mempool Protection

| Attack Type | Countermeasure | Parameter |
|---------|---------|------|
| Transaction flood | Minimum fee threshold + dynamic rejection | Directly reject below dynamic threshold |
| Single address pool fill | Per-address Pending limit | 256 tx/address |
| Large transaction attack | Transaction size limit | 256 KB |
| Precompile call inflation | Call depth limit | 1,024 frames/tx |
| Batch transfer inflation | Batch payment recipient limit | 5,000 recipients/tx |
| Signature forgery | Immediate verification and discard | Invalid signatures never enter mempool |
| Replay attack | Nonce check | Stale nonce immediately rejected |
| Shielded proof inflation | Proof size validation | ZK proofs >1KB rejected |
| Agent sub-account abuse | Permission + limit checks | Daily limit + per-transaction limit |

```rust
/// Mempool admission check
fn accept_tx(tx: &EvmTx) -> Result<()> {
    // 1. Transaction size check
    let tx_size = alloy_rlp::encode(tx).len();
    ensure!(tx_size <= BLOCK_LIMITS.max_tx_size as usize, "Tx too large");

    // 2. Gas limit check
    ensure!(tx.gas_limit <= BLOCK_LIMITS.max_evm_gas_per_block, "Gas limit too high");

    // 3. Minimum fee check (max_fee_per_gas >= current base_fee)
    let current_base_fee = get_current_base_fee();
    ensure!(tx.max_fee_per_gas >= current_base_fee, "Fee too low");

    // 4. Per-address Pending limit
    let sender = recover_sender(tx)?;
    let pending_count = mempool.count_pending(&sender);
    ensure!(pending_count < 256, "Pending limit exceeded");

    // 5. Signature verification (standard EVM recovery)
    let recovered = recover_sender(tx)?;
    ensure!(recovered != Address::zero(), "Invalid signature");

    // 6. Nonce check
    ensure!(tx.nonce == get_evm_nonce(&sender), "Invalid nonce");

    // 7. Pool capacity check
    ensure!(mempool.evm_pool.len() < 100_000, "Pool full");

    Ok(())
}
```

#### 13.5.3 Shielded Pool Protection

| Risk | Protection |
|------|------|
| ZK proof DoS (大量 invalid proofs) | Transactions with failed verification immediately dropped + small proof verification fee charged (even on failure) |
| Nullifier set膨胀 | Compressed storage using BitSet, 100M entries ~12.5 MB |
| Merkle Tree depth attack | Incremental Merkle Tree depth limit 32, automatic rejection |
| Large shielded transfer money laundering | Compliance mode (KYC/Whitelist) configured by asset issuer |
| Per-block Shielded flood | Per-block limit of 50, excess queued |

#### 13.5.4 P2P Network Protection

```rust
struct NetworkLimits {
    /// Maximum peer connections
    max_peers: u32,                  // default 50

    /// Maximum message rate per connection
    max_messages_per_second: u32,    // default 100

    /// Maximum message size
    max_message_size: u32,           // default 10 MB

    /// Known transaction deduplication cache size
    known_txs_cache_size: u32,       // default 1,000,000

    /// Malicious node ban duration
    ban_duration_seconds: u64,       // default 3600 (1 hour)
}
```

| Attack Type | Protection |
|---------|------|
| Sybil attack (大量 fake nodes) | Connection limit + node reputation system, abnormal connections automatically disconnected |
| Message flood | Per-connection rate limiting, pause 1 hour when threshold exceeded |
| Large message DoS | 10 MB message limit, immediately disconnect if exceeded |
| Block/transaction retransmission | Deduplication cache, known hashes not reprocessed |
| Eclipse attack | Maintain ≥8 outbound connections to different subnets |
| Route hijacking | commonware-p2p supports TLS encrypted channels + node ID verification |

#### 13.5.5 Consensus Layer Protection

| Attack Type | Protection |
|---------|------|
| 51% attack | Simplex BFT tolerates <1/3 Byzantine nodes, requires >2/3 honest to produce blocks |
| Double-sign attack | Double-sign detected → automatic slash of all self-stake |
| Validator offline | Proportional stake deduction based on offline rounds, 100 consecutive rounds offline → removed from validator set |
| Long-range attack | Light clients only trust recent checkpoints, old forks automatically rejected |
| Nothing-at-Stake | Per-round subset rotation, cannot know next round's proposer in advance |

#### 13.5.6 Economic Protection Summary

| Layer | Protection Mechanism | Cost Model |
|------|---------|---------|
| Transaction layer | Fee anti-spam | Each transaction requires CALL payment |
| Mempool | Dynamic minimum fee + capacity limits | Low-fee transactions rejected |
| Network layer | Rate limiting + connection limits | Attacker bandwidth cost grows linearly |
| Consensus layer | Stake slashing | Malicious behavior loss > gain |
| Shielded | Proof verification fee + per-block cap | ZK proof generation cost is high |
| Governance layer | Proposal deposit | Spam proposals lose 10,000 CALL |

---

## 14. Performance Targets

| Metric | Target Value |
|------|--------|
| TPS | 5,000+ |
| Block time | 250ms |
| Finality | ~500ms |
| Protocol payment latency | < 10ms (protocol layer) |
| Bridge latency | < 1 block (< 250ms) |
| Node hardware requirements | 4 cores / 8GB / 500GB SSD |

---

## 15. Rust Crate Structure

```
call-core/
├── Cargo.toml
├── crates/
│   ├── primitives/        # Base types (Address, Hash, AssetId, Balance)
│   ├── crypto/            # Cryptography (secp256k1, ed25519, SHA-256)
│   ├── serialization/     # Protocol serialization
│   ├── protocol/          # Protocol payment layer
│   │   ├── registry/      # Asset registry
│   │   ├── balances/      # Balance management
│   │   ├── compliance/    # Compliance policy engine
│   │   └── payment/       # Payment transaction execution
│   ├── agent/             # Agent payment layer
│   │   ├── registry/      # Agent registration and identity verification
│   │   ├── permissions/   # Permissions and fee configuration
│   │   ├── balances/      # Agent sub-account balances
│   │   └── executor/      # Agent transaction execution and fee sponsorship
│   ├── shielded/          # Shielded Pool privacy layer
│   │   ├── merkle/        # Incremental Merkle Tree management
│   │   ├── notes/         # Note creation, encryption, storage
│   │   ├── nullifiers/    # Nullifier set and anti-double-spend
│   │   ├── circuit/       # ZK circuit definitions and parameter management
│   │   ├── prover/        # Proof generation (client) and verification (node)
│   │   └── compliance/    # View key management and compliance auditing
│   ├── evm/               # EVM smart contract layer
│   │   ├── executor/      # EVM executor (Revm)
│   │   ├── precompiles/   # Precompiled contracts (bridge, protocol balances)
│   │   └── contracts/     # System contracts (ERC-20 template, bridge)
│   ├── bridge/            # Internal bridge
│   │   ├── deposit/       # Protocol → EVM
│   │   ├── withdraw/      # EVM → Protocol
│   │   └── sync/          # Cross-layer state synchronization
│   ├── consensus/         # Consensus layer (Commonware Simplex)
│   │   ├── simplex/       # Simplex state machine integration
│   │   ├── proposer/      # Proposer selection and subset rotation
│   │   └── validator/     # Validator management and staking
│   ├── network/           # P2P network (commonware-p2p)
│   ├── storage/           # Storage layer (reth-db)
│   ├── rpc/               # RPC server (jsonrpsee)
│   └── node/              # Node application and CLI
└── tests/
    ├── integration/       # Integration tests
    └── e2e/              # End-to-end tests
```

---

## 16. Genesis

### 16.1 Genesis Block

The genesis block is the chain's initial state, at height 0, with no parent block.

```rust
struct Genesis {
    chain_id: u64,
    timestamp: u64,
    initial_validators: Vec<ValidatorInfo>,
    initial_assets: Vec<GenesisAsset>,
    consensus_params: ConsensusParams,
}

struct ValidatorInfo {
    id: ValidatorId,
    public_key: PublicKey,
    consensus_key: Ed25519PublicKey,
    stake: u128,
    metadata: ValidatorMetadata,
}

struct GenesisAsset {
    name: String,
    symbol: String,
    decimals: u8,
    issuer: Address,
    initial_supply: u128,
    policy: CompliancePolicy,
}

struct ConsensusParams {
    max_validators: u32,
    subset_size: u32,
    block_time_millis: u64,
    slashing_window: u64,
}
```

### 16.2 Genesis Format

The genesis configuration is provided in JSON format:

```json
{
    "chain_id": 1,
    "timestamp": 1744502400000,
    "initial_validators": [
        {
            "id": 1,
            "public_key": "0x...",
            "consensus_key": "ed25519:...",
            "stake": "1000000000000000000",
            "metadata": { "name": "Validator 1", "url": "https://..." }
        }
    ],
    "initial_assets": [
        {
            "name": "Callchain Token",
            "symbol": "CALL",
            "decimals": 18,
            "issuer": "0x...",
            "initial_supply": "1000000000000000000000000000",
            "policy": "None"
        }
    ],
    "consensus_params": {
        "max_validators": 216,
        "subset_size": 21,
        "block_time_millis": 250,
        "slashing_window": 10000
    },
    "initial_fee_currencies": [
        {
            "asset_symbol": "CALL",
            "oracle_price_key": "CALL_USD"
        }
    ],
    "fee_params": {
        "initial_base_fee": 10,
        "target_gas_per_block": 10000000,
        "max_gas_per_block": 20000000,
        "stablecoin_cap_bps": 5000
    }
}
```

### 16.3 Startup Process

```
1. Parse genesis.json
2. For each GenesisAsset:
   - If CALL (asset_id = 1): set native EVM balance only (`evm_state.set_balance`)
   - Otherwise: set both protocol-layer balance and native EVM balance
3. Register initial validator set
4. Register initial Gas payment currencies to FeeCurrencyRegistry
5. Create genesis block (height=0, parent_hash=0x0)
6. Compute initial state root (EVM state root)
7. Nodes begin running consensus from height 0
```

---

## 17. Transaction Pool (Mempool)

### 17.1 Design

The transaction pool maintains EVM transactions waiting to be packed. All transactions are standard EVM transactions (which may include precompile calls).

```rust
struct Mempool {
    evm_pool: PriorityTxs<EvmTx>,        // EVM transactions (including precompile calls)
    known_txs: LruCache<TxHash, ()>,     // deduplication cache
}
```

### 17.2 Priority and Sorting

| Dimension | Strategy |
|------|------|
| EvmTx sorting | Descending by `max_priority_fee_per_gas` + timestamp FIFO |

**Priority score:**
```rust
fn priority_score(tx: &EvmTx) -> u128 {
    tx.max_priority_fee_per_gas
}
```

### 17.3 Capacity and Eviction

| Parameter | Value | Description |
|------|------|------|
| evm_pool limit | 100,000 txs | Dynamically adjusted based on gas limit |
| Per-address Pending limit | 256 txs | Prevent single address from filling pool |
| Minimum gas price | Dynamic | Automatically evicted below threshold |
| Lifetime | 72 blocks | Evicted if not packed within timeout |

**Eviction Strategy:**
1. Transactions with gas price (CALL equivalent) below current minimum acceptance threshold are evicted first
2. Stale nonce (expired) transactions immediately cleared
3. When pool is full, evict from the tail by priority

### 17.4 Anti-Spam Mechanism

- EvmTx: Minimum gas price requirement (including precompile calls)
- Stablecoin Gas payment: Must be in FeeCurrencyRegistry with valid oracle price
- Duplicate transaction detection: Known TxHash directly rejected
- Invalid signature transactions immediately dropped and logged

---

## 18. State Transition

### 18.1 Formal Definition

```
State = EvmState   // All state (balances, assets, validators, etc.) lives in EVM storage

apply_block(state, block) -> Result<State> {
    state = execute_evm_txs(state, block.evm_txs)?;
    // Precompile calls inside evm_txs read/write protocol state from EVM storage
    // via StorageRef. Revm's Journal guarantees per-transaction atomicity.
    Ok(state)
}
```

### 18.2 Transaction Validity Rules

**EvmTx validation:**
- Standard secp256k1 signature valid (Ethereum-compatible recovery)
- nonce >= account's current nonce
- Sender balance >= gas_limit * max_fee_per_gas + value
- gas_limit <= block gas limit
- For precompile calls: precompile address in allowed range (0x101–0x209)
- For precompile calls: caller has required permissions (issuer, agent owner, etc.)

**EvmTx validation:**
- Signature valid (secp256k1, Ethereum compatible)
- nonce >= account's current nonce
- Sender balance >= gas_limit * gas_price + value
- gas_limit <= block gas limit

**Shielded precompile validation:**
- ZK proof verification passes (Halo2 IPA)
- All nullifiers unspent (anti-double-spend)
- Merkle Tree root matches (input Notes actually exist)
- Asset exists and Shielded functionality is enabled
- ShieldedDeposit: sender transparent balance >= deposit amount
- ShieldedWithdraw: amount publicly revealed in proof, balance restored to target address
- ShieldedTransfer: proof implies input >= output (without exposing specific values)

### 18.3 Atomicity Guarantee

All operations within a block either all succeed or all roll back:
- EVM transactions: Revm's Journal mechanism guarantees single-tx atomicity. Precompile calls share this guarantee because protocol state lives in EVM storage accessed via `StorageRef`.
- Block level: State root computed after all EVM transactions, inconsistent results reject the block

### 18.4 Transaction Receipts

After each transaction executes, a receipt is generated and packed into the block. Receipts are the sole source for block explorer queries, contract log reading, and event monitoring.

#### 18.4.1 EVM Transaction Receipt (including Precompile Calls)

All transactions produce standard Ethereum-compatible receipts. Precompile calls emit EVM logs and return data just like regular contract calls.

```rust
/// EVM transaction receipt (standard Ethereum format)
struct EvmReceipt {
    tx_hash: Hash,
    status: bool,               // true = success, false = reverted
    gas_used: u64,
    contract_address: Option<Address>,  // if contract creation
    logs: Vec<EvmLogEntry>,
    logs_bloom: Bloom,          // Bloom filter (fast log filtering)
}

struct EvmLogEntry {
    address: Address,           // precompile address or contract address
    topics: Vec<H256>,          // indexed fields
    data: Vec<u8>,              // non-indexed data
}
```

**Precompile event logs:** Protocol precompiles emit logs with their own address (e.g., `0x201` for transfers). Wallets and indexers can filter by precompile address to track protocol operations.

```rust
// Example: transfer precompile emits a log
EvmLogEntry {
    address: Address::from_hex("0x201"),
    topics: vec![
        keccak256("Transfer(uint64,address,address,uint128)"),
        H256::from_low_u64_be(asset_id),
        H256::from(sender),
        H256::from(to),
    ],
    data: encode_abi_uint(amount),
}
```
```

#### 18.4.2 Shielded Transaction Receipt

Shielded transaction receipts need to hide sensitive information (amounts, participants) while ensuring verifiability:

```rust
struct ShieldedReceipt {
    tx_hash: Hash,
    status: ExecutionStatus,
    gas_used: u128,
    gas_payer: Address,

    // Public information
    nullifiers: Vec<Nullifier>,    // public (anti-double-spend)
    commitments: Vec<NoteCommitment>, // public (Merkle Tree update)

    // Hidden information (only view key holders can read)
    encrypted_event: Option<Vec<u8>>, // encrypted event data
    // Not exposed: sender, receiver, amount
}
```

Characteristics of Shielded receipts:
- `nullifiers` and `commitments` are public, used for Merkle Tree state maintenance
- Amounts, senders, receivers are not written to the receipt
- `encrypted_event` is optional, contains encrypted details, only decryptable by view key holders
- Block explorers only show "a Shielded operation occurred", not the specifics

#### 18.4.3 External Bridge Transaction Receipt

```rust
struct ExternalBridgeReceipt {
    tx_hash: Hash,
    status: ExecutionStatus,
    gas_used: u128,
    bridge_op: ExternalBridgeOp,

    // Bridge-specific information
    source_tx_hash: Option<Hash>,     // source chain transaction hash
    source_block: Option<u64>,        // source chain block height
    confirmations: Option<u32>,       // source chain confirmations
}

#### 18.4.3 External Bridge Transaction Receipt

```rust
struct ExternalBridgeReceipt {
    tx_hash: Hash,
    status: ExecutionStatus,
    gas_used: u128,
    bridge_op: ExternalBridgeOp,

    // Bridge-specific information
    source_tx_hash: Option<Hash>,     // source chain transaction hash
    source_block: Option<u64>,        // source chain block height
    confirmations: Option<u32>,       // source chain confirmations
}
```

#### 18.4.5 Block Receipt Tree

Receipts are indexed by block number and transaction hash in the RPC layer.
The block header contains a single `state_root` (EVM state root); receipt integrity
is maintained through the receipts database, not a Merkle root in the header.

```rust
/// Receipts are stored in the database and served via RPC
/// Block header has a single state_root — no separate receipt_root
struct BlockHeader {
    parent_hash: Hash,
    height: u64,
    timestamp_millis: u64,
    state_root: Hash,               // EVM state root
    proposer: ValidatorId,
    signature: Signature,
    version: ProtocolVersion,
    bls_aggregate_signature: Option<Vec<u8>>,
    bls_signer_bitmap: Vec<u8>,
}
```

#### 18.4.6 Receipt Query (RPC Interface)

```json
// Query receipt by transaction hash
{
    "method": "call_getTransactionReceipt",
    "params": ["0xabc..."],
    "id": 1
}
→ {
    "tx_hash": "0xabc...",
    "block_height": 42,
    "status": "success",
    "gas_used": "15000",
    "gas_payer": "0x123...",
    "logs": [
        { "address": "0x...", "topics": ["..."], "data": "..." }
    ],
    "state_changes": [
        { "asset_id": 1, "address": "0x123...", "type": "balance", "before": "100", "after": "90" }
    ]
}

// Query all receipts by block height
{
    "method": "call_getBlockReceipts",
    "params": [42],
    "id": 1
}
→ [{ receipt_1, receipt_2, ... }]

// Filter logs by address
{
    "method": "call_getLogs",
    "params": [{ address: "0x...", topics: ["..."], from_block: 0, to_block: "latest" }],
    "id": 1
}
→ [{ log_1, log_2, ... }]

// EVM-compatible query (eth_getTransactionReceipt)
{
    "method": "eth_getTransactionReceipt",
    "params": ["0xabc..."],
    "id": 1
}
→ Standard Ethereum receipt format

// Query transaction by memo reference number
{
    "method": "call_getTxByReference",
    "params": ["PAYROLL-2026-03-001"],
    "id": 1
}
→ [{ tx_hash, block_height, memo, status, timestamp }]
```

#### 18.4.7 Receipt Prune Strategy

Receipt data grows quickly but is primarily used for historical queries. Prune strategy see §10.3:

```
- Recent receipts (most recent keep_receipt blocks): fully stored
- Historical receipts: pruned after keep_receipt
- Receipts are stored in the database and pruned per node mode; no receipt root in block header
- After pruning, receipts can still be verified as belonging to a block via Merkle proof
```

| Node Mode | Receipt Retention |
|----------|---------|
| Validator | Most recent 1 million blocks |
| Full | Most recent 1 million blocks |
| Light | None (query full nodes on demand) |
| Archive | All history |

---

## 19. Fork/Upgrade

### 19.1 Protocol Version

```rust
struct ProtocolVersion {
    major: u32,   // incompatible changes
    minor: u32,   // backward-compatible features
    patch: u32,   // bug fixes
}
```

### 19.2 Upgrade Mechanism Options

| Mechanism | Option A: Height Activation | Option B: Signal Voting | Option C: Governance Proposal |
|------|------------------|------------------|------------------|
| Trigger | Preset block height | 2/3 validator signal | On-chain governance proposal passed |
| Flexibility | Low (requires advance planning) | Medium | High |
| Security | High (deterministic) | Medium | High |
| Complexity | Lowest | Medium | High |
| **Recommended** | ✅ For planned upgrades | ⚠️ For emergency upgrades | ✅ Long-term governance direction |

**Current choice: Height activation + governance proposal dual-track system**
- Planned upgrades: Preset block height, all nodes upgrade in sync
- Major changes: Via on-chain governance proposal (2/3 validator vote) + timelock execution

### 19.3 Upgrade Process

```
1. Proposal: Submit upgrade proposal (includes new version, activation height, changelog)
2. Voting: Validators vote within 7 days, requires 2/3 majority
3. Timelock: 7-day timelock after passing, giving nodes time to upgrade
4. Activation: New version rules take effect at preset height
5. Non-upgraded nodes: Automatically stop producing blocks (version check fails)
```

### 19.4 Rollback Strategy

- Critical bugs discovered within 100 blocks after upgrade: 2/3 validator signatures can trigger emergency pause
- After pause, network stops producing blocks until issue is fixed
- No automatic rollback (would break finality), requires coordinated restart

---

## 20. Telemetry/Metrics

### 20.1 Metrics System

```rust
// Prometheus metrics
metrics: {
    // Consensus layer
    call_consensus_round_duration_seconds: Histogram,
    call_consensus_rounds_total: Counter,
    call_consensus_proposals_received: Counter,
    call_consensus_votes_received: Counter,
    call_consensus_validator_set_size: Gauge,

    // Transaction layer
    call_mempool_size: GaugeVec,          // by type
    call_transactions_processed_total: CounterVec, // by type/status
    call_transaction_execution_time_seconds: Histogram,

    // P2P Network
    call_p2p_peers: Gauge,
    call_p2p_messages_sent_total: CounterVec,
    call_p2p_messages_received_total: CounterVec,
    call_p2p_bandwidth_bytes: CounterVec,

    // Performance
    call_block_height: Gauge,
    call_block_processing_time_seconds: Histogram,
    call_state_root_computation_time_seconds: Histogram,

    // System
    call_process_cpu_seconds: Counter,
    call_process_memory_bytes: Gauge,
    call_process_open_fds: Gauge,
}
```

### 20.2 Integration

- **Prometheus**: Exposes `/metrics` endpoint at `:9090` by default
- **OpenTelemetry**: Optional integration, supports Jaeger/Zipkin distributed tracing
- **Grafana Dashboards**: Pre-built dashboards
  - Consensus Overview: Round times, vote rates, validator activity
  - Transaction Throughput: TPS, latency, pool size
  - Bridge Operations: Deposit/withdrawal volume, latency
  - System Health: CPU, memory, disk, network

### 20.3 Alert Rules

| Alert | Condition | Severity |
|------|------|------|
| Consensus stall | No new block for 60 seconds | Critical |
| Validator offline | Validator has not voted for 100 rounds | Warning |
| Transaction pool overflow | Pool utilization > 90% | Warning |
| Bridge delay | Pending bridges > 100 | Warning |
| Memory overflow | RSS > 6GB | Critical |
| Disk space | Available space < 50GB | Warning |

---

## 21. Node Startup and Configuration (Boot/Config)

### 21.1 CLI Arguments

```bash
callchain-node [OPTIONS]

Consensus layer:
    --genesis <PATH>              Path to genesis configuration file (required)
    --validator                   Run in validator mode
    --validator-key <HEX>         Validator private key
    --consensus-key <HEX>         Consensus ed25519 private key
    --peers <ADDRS>               Initial seed nodes, comma-separated

Network layer:
    --p2p-listen <ADDR>           P2P listen address (default 0.0.0.0:51235)
    --p2p-advertise <ADDR>        Public advertise address
    --max-peers <N>               Maximum peer connections (default 50)

RPC layer:
    --rpc-http-addr <ADDR>        HTTP RPC address (default 127.0.0.1:8545)
    --rpc-ws-addr <ADDR>          WebSocket address (default 127.0.0.1:8546)
    --rpc-cors <ORIGINS>          CORS allowed origins

Storage layer:
    --data-dir <PATH>             Data directory (default ~/.callchain)
    --db-cache-size <MB>          Database cache size (default 1024)

Monitoring layer:
    --metrics-addr <ADDR>         Prometheus address (default 0.0.0.0:9090)
    --tracing                     Enable OpenTelemetry tracing

Logging layer:
    --log-level <LEVEL>           Log level (default info)
    --log-format <FORMAT>         Log format: json|text (default text)
```

### 21.2 Configuration File (TOML)

```toml
[chain]
chain_id = 1
genesis_file = "genesis.json"

[consensus]
validator = true
validator_key_file = "keys/validator.pem"
consensus_key_file = "keys/consensus.pem"

[network]
listen_addr = "0.0.0.0:51235"
advertise_addr = "public-ip:51235"
bootstrap_nodes = [
    "/dns4/seed1.callchain.cc/tcp/51235/p2p/...",
    "/dns4/seed2.callchain.cc/tcp/51235/p2p/...",
]

[rpc]
http_addr = "0.0.0.0:8545"
ws_addr = "0.0.0.0:8546"
cors_origins = ["*"]

[storage]
data_dir = "/var/lib/callchain"
db_cache_size = 2048  # MB

[metrics]
enabled = true
addr = "0.0.0.0:9090"

[logging]
level = "info"
format = "json"
file = "/var/log/callchain/node.log"
```

### 21.3 Startup Process

```
1. Parse CLI arguments + configuration file (CLI takes priority)
2. Initialize logging system
3. Open/create database (reth-db)
4. Load genesis configuration, initialize state
   - If database is empty: perform genesis initialization
   - If database has data: restore from last state
5. Initialize P2P network (commonware-p2p)
6. Connect to seed nodes, establish peer connections
7. Initialize consensus engine (Simplex)
8. Start RPC server (HTTP + WebSocket)
9. Start monitoring endpoint (Prometheus)
10. Begin block synchronization / participate in consensus
```

---

## 22. State Expiration

### 22.1 Design Options

| Model | Option A: No State Expiration | Option B: State Rent | Option C: Auto Expiration |
|------|-------------------|------------------|------------------|
| State growth | Infinite growth | Requires maintenance fee | Auto-clear on timeout |
| User burden | None | Periodic payments | Requires periodic activity |
| Node burden | Continuous growth | Controllable | Controllable |
| Implementation complexity | Lowest | High | Medium |
| **Recommended** | ✅ Initial phase | ⚠️ Long-term goal | ❌ Not suitable for asset chain |

**Current choice: No state expiration (initial phase)**

Rationale: The core value of the protocol payment layer is deterministic balance mapping, and state expiration would break this guarantee. Initially adopting **no state expiration**, protocol-layer balances and bridge state never expire.

### 22.2 EVM Layer State Management

The EVM layer follows Ethereum EIP-161 rules:
- Empty accounts (nonce=0, balance=0, code_hash=empty) are automatically cleared after transactions
- Contract storage slots with zero values are not written to disk
- EIP-7742 (stateful expiration) may be considered as an upgrade path in the future

### 22.3 Storage Optimization

- State pruning: Retain state for the most recent N blocks, earlier state reconstructed via Merkle proofs
- Snapshot compression: Periodically create state snapshots, delete old historical data
- Optional archive nodes: Archive node mode providing full historical queries

### 22.4 Relationship Between State Expiration and Pruning

State expiration (§22) and data pruning (§10.3) address different layers of the problem but work together:

```
State Expiration
  → Solves "which logical data should be removed from the ledger"
  → Protocol semantics layer: Should an account be retained after N days of inactivity
  → Determines what data "no longer has meaning"

Data Pruning
  → Solves "how much historical data the node keeps on disk"
  → Storage implementation layer: Even meaningful data doesn't require all intermediate state
  → Determines what data "no longer needs to be stored locally"

Relationship:
  1. State expiration reduces pruning workload (expired data naturally doesn't need pruning)
  2. Pruning can be more aggressive than state expiration (current balances don't expire, but historical intermediate state can be pruned)
  3. Together they ensure node storage grows controllably
```

| Data Type | State Expiration Policy | Prune Strategy |
|----------|------------|-----------|
| Protocol-layer balances | Never expire | Current value retained, historical versions pruned |
| Shielded nullifiers | Never expire | Never pruned (required for anti-double-spend) |
| Agent registration info | Never expire (unless Revoked) | Current value retained |
| EVM empty accounts | Auto-cleared (EIP-161) | Storage naturally freed after clearing |
| Historical transaction traces | Not applicable | Pruned after keep_recent |
| Block body (transaction details) | Not applicable | Pruned after keep_block_body |

---

## 23. Light Client

### 23.1 Protocol

Light clients do not store the full state, only verify block headers:

```rust
struct LightClient {
    trusted_validators: HashMap<ValidatorId, PublicKey>,
    latest_block_header: BlockHeader,
    chain_id: u64,
}

impl LightClient {
    /// Verify new block header
    fn verify_header(&mut self, header: &BlockHeader) -> Result<()> {
        // 1. Verify parent hash linkage
        ensure!(header.parent_hash == self.latest_block_header.hash());

        // 2. Verify 2/3+ validator signatures
        let signatures = header.aggregate_signature;
        let voting_power = self.calculate_voting_power(&signatures);
        ensure!(voting_power > self.total_voting_power() * 2 / 3);

        // 3. Verify state root consistency
        self.latest_block_header = header.clone();
        Ok(())
    }

    /// Verify Merkle proof
    fn verify_proof<T: MerkleProof>(
        &self,
        proof: &T,
        root: Hash,
    ) -> Result<T::Value> {
        proof.verify(root)
    }
}
```

### 23.2 Supported Operations

| Operation | Method |
|------|------|
| Verify block header | `light_verifyBlockHeader` |
| Protocol balance proof | `call_getBalanceProof(asset_id, address)` → MerkleProof |
| EVM balance proof | `eth_getProof(address, storageKeys, blockNumber)` |
| Transaction inclusion proof | `call_getTransactionProof(tx_hash)` → MerkleProof |
| Bridge operation proof | `call_getBridgeProof(op_hash)` |
| Shielded Pool state proof | `call_getShieldedStateProof(asset_id)` → ShieldedStateProof |
| Shielded balance query | `call_getShieldedBalanceProof(viewing_key)` → EncryptedBalanceProof |
| Shielded transaction inclusion | `call_getShieldedTxProof(tx_hash)` → ShieldedMerkleProof |

### 23.3 Shielded Pool Light Client Verification

Light client support for the Shielded Pool is divided into two categories:

**Full validation mode (verifies ZK proofs):**
```rust
/// Light client verifies ShieldedTransfer
/// Requires downloading and verifying ZK proofs (computationally intensive, highest security)
fn verify_shielded_tx_full(&self, tx: &ShieldedTransfer) -> Result<()> {
    // 1. Verify ZK proof (Halo2 IPA ~5-10ms)
    verify_zk_proof(&tx.proof)?;

    // 2. Verify nullifiers unspent (requires full node proof)
    let nullifier_proof = request_nullifier_proof(tx.nullifiers)?;
    ensure!(verify_merkle_proof(&nullifier_proof));

    // 3. Verify commitments are on-chain
    let commit_proof = request_commitment_proof(tx.commitments)?;
    ensure!(verify_merkle_proof(&commit_proof));

    Ok(())
}
```

**Simplified mode (trust node summary, suitable for mobile):**
```rust
/// Light client only verifies Shielded state summary proof
/// Does not verify ZK proof itself, only verifies "validators have verified this transaction"
fn verify_shielded_tx_light(&self, tx_hash: Hash) -> Result<()> {
    // 1. Request full node to provide Shielded transaction Merkle inclusion proof
    let proof = request_shielded_merkle_proof(tx_hash)?;

    // 2. Verify transaction is indeed included in the verified block
    ensure!(proof.verify(self.latest_block_header.shielded_root));

    // 3. Verify 2/3 validators have signed this block header
    // (implicitly validators have verified the ZK proof)
    Ok(())
}
```

**Shielded Balance Query (via Viewing Key):**
```rust
/// Light client queries Shielded balance using viewing key
fn query_shielded_balance(
    &self,
    viewing_key: &ViewingKey,
    asset_id: AssetId,
) -> Result<u128> {
    // 1. Request full node: decrypt relevant Notes using viewing key
    let notes = request_shielded_notes(viewing_key, asset_id)?;

    // 2. Each Note includes Merkle inclusion proof
    for note in &notes {
        ensure!(note.proof.verify(self.latest_block_header.shielded_root));
    }

    // 3. Sum = balance (only view key holder can decrypt)
    Ok(notes.iter().map(|n| n.value).sum())
}
```

### 23.4 Sync Strategy

| Phase | Description |
|------|------|
| Initial sync | Start from trusted checkpoint, verify each block header |
| Incremental sync | Verify new block header signatures and state roots one by one |
| State sync | Request Merkle proofs from full nodes on demand |

Light client resource usage:
- Storage: Block headers + validator set only (< 10MB)
- Bandwidth: ~1KB block header per block
- Computation: Signature verification per block (216 validators ~50ms)

---

## 24. Logging/Auditing

### 24.1 Structured Logging

```rust
// Example log entry
struct LogEntry {
    timestamp: String,        // ISO 8601
    level: LogLevel,          // trace, debug, info, warn, error
    target: String,           // module path
    message: String,
    fields: HashMap<String, Value>,  // structured fields
}

// Example output (JSON format)
{
    "timestamp": "2026-04-13T10:30:00.123Z",
    "level": "info",
    "target": "callchain_protocol::payment",
    "message": "Payment transaction executed",
    "fields": {
        "tx_hash": "0xabc...",
        "asset_id": 42,
        "from": "0x123...",
        "to": "0x456...",
        "amount": "1000000000000000000",
        "fee": "100000000000",
        "duration_ms": 2
    }
}
```

### 24.2 Audit Log

Unlike regular logs, audit logs are immutable append-only logs recording all state changes:

```rust
struct AuditEntry {
    block_height: u64,
    tx_index: u32,
    tx_type: String,        // "payment", "agent", "evm", "bridge", "system", "shielded"
    action: String,         // "transfer", "agent_pay", "mint", "burn", "deposit", "withdraw",
                            // "shielded_transfer", "shielded_deposit", "shielded_withdraw"
    agent_id: Option<u64>,  // Recorded for Agent transactions
    fee_payer: Option<String>, // "self", "owner", "third_party" (for Agent transactions)
    before_state: StateSnapshot,  // relevant state before change
    after_state: StateSnapshot,   // relevant state after change
    tx_hash: Hash,
    shielded_details: Option<ShieldedAuditInfo>, // shielded transaction audit (only view key holders can read)
}

/// Shielded transaction audit info (encrypted storage)
struct ShieldedAuditInfo {
    nullifiers: Vec<Nullifier>,
    commitments: Vec<NoteCommitment>,
    encrypted_amounts: Vec<EncryptedValue>,  // only decryptable by view key holders
    compliance_mode: String,                 // "unrestricted", "kyc_required", ...
}
```

Audit log storage:
- Independent database table (`audit_log`), append-only, never deleted
- Periodically Merkle-ized, root hash written to block header (optional, for third-party audit verification)

### 24.3 Compliance Report API

```json
// Export compliance report for a specific time range
{
    "method": "call_exportComplianceReport",
    "params": [{
        "asset_id": 42,
        "from_timestamp": "2026-01-01T00:00:00Z",
        "to_timestamp": "2026-04-01T00:00:00Z",
        "addresses": ["0x123...", "0x456..."],
        "format": "csv"
    }],
    "id": 1
}
→ { report_url: "https://.../report.csv", expires_at: "..." }
```

### 24.4 Log Configuration

| Parameter | Options | Default Value |
|------|------|--------|
| Log level | trace, debug, info, warn, error | info |
| Log format | json, text | text |
| Log output | stdout, file, both | stdout |
| Log rotation | By size (100MB) or daily | Daily |
| Log retention | 30 days (configurable) | 30 days |
| Audit log | Always enabled | - |

---

## 25. Oracle

### 25.1 Design

Callchain natively supports a validator feed price system, where a consensus subset of validators periodically submits price data, with the median taken as the official price. Price data is exposed to DeFi contracts for direct reading via an EVM precompiled contract.

### 25.2 Core Data Structures

```rust
/// Validator price submission
struct OracleSubmission {
    asset_id: AssetId,
    price: u128,              // denominated in CALL, scaled by 18 decimals
    timestamp: u64,
    validator_id: ValidatorId,
    signature: Signature,
}

/// Aggregated price (median)
struct AggregatedPrice {
    asset_id: AssetId,
    median_price: u128,       // median price
    valid_submissions: u32,    // number of valid submissions
    timestamp: u64,            // update timestamp
    block_updated: u64,        // updating block height
}

/// Validator oracle state
struct OracleValidatorInfo {
    submission_count: u32,     // cumulative submissions
    outlier_count: u32,        // times deviating >5% from median
    is_active: bool,           // eligible to submit
    last_submission: u64,      // last submission timestamp
}

/// Historical price (for TWAP)
struct HistoricalPrice {
    price: u128,
    timestamp: u64,
}
```

### 25.3 Price Submission Process

```
Each price update cycle (every 1000 blocks ≈ 4 minutes):

1. Validator obtains price from external API
   → Data sources: CoinGecko, Binance, Coinbase (at least 2 independent sources)
   → Validator locally computes: takes median of multiple sources

2. Validator signs and submits
   → Signs with secp256k1 key: sign(hash(asset_id, price, timestamp))
   → Submits to Oracle system contract

3. System contract collects submissions
   → Waits for 2/3 validator submissions (14/21)
   → Excludes outliers (submissions deviating >5% from median)
   → Calculates median as official price

4. Price written to state
   → AggregatedPrice updated
   → Historical record appended to TWAP queue
   → EVM precompiled contract automatically exposes latest price

5. Anomalous validators flagged
   → outlier_count +1
   → 10 consecutive anomalies → lose submission eligibility
```

```rust
/// Validator submits price
fn submit_oracle_price(
    submission: OracleSubmission,
) -> Result<()> {
    let validator = get_validator_info(submission.validator_id)?;

    // 1. Verify submission eligibility
    ensure!(validator.oracle_info.is_active, "Oracle submission disabled");

    // 2. Verify signature
    verify_signature(
        &validator.consensus_key,
        &submission.signature,
        &submission.hash()
    )?;

    // 3. Verify time window (within current cycle)
    let current_period = current_block_height() / ORACLE_UPDATE_INTERVAL;
    let submission_period = submission.timestamp / ORACLE_PERIOD_SECS;
    ensure!(submission_period == current_period, "Wrong submission period");

    // 4. Anti-replay (each validator can only submit once per cycle)
    ensure!(
        !OracleSubmissions::has_submitted(
            submission.validator_id,
            submission.asset_id,
            current_period
        ),
        "Already submitted this period"
    );

    // 5. Record submission
    OracleSubmissions::insert(
        submission.validator_id,
        submission.asset_id,
        current_period,
        submission.price,
    );

    OracleValidatorInfo::record_submission(submission.validator_id);

    // 6. If sufficient submissions collected, trigger aggregation
    let submission_count = OracleSubmissions::count_for_asset(submission.asset_id, current_period);
    if submission_count >= oracle_quorum() {
        aggregate_and_publish_price(submission.asset_id, current_period)?;
    }

    Ok(())
}

/// Aggregate price: take median, exclude outliers
fn aggregate_and_publish_price(asset_id: AssetId, period: u64) -> Result<()> {
    let submissions = OracleSubmissions::get_all(asset_id, period);

    // Sort
    let mut prices: Vec<u128> = submissions.iter().map(|s| s.price).collect();
    prices.sort();

    // Calculate median
    let median = prices[prices.len() / 2];

    // Mark outliers (deviating >5% from median)
    for submission in &submissions {
        let deviation = ((submission.price as i128 - median as i128).abs() as u128) * 100 / median;
        if deviation > 5 {
            OracleValidatorInfo::mark_outlier(submission.validator_id);
        }
    }

    // Check if validator should be disabled due to repeated anomalies
    for submission in &submissions {
        let info = OracleValidatorInfo::get(submission.validator_id);
        if info.outlier_count >= 10 {
            info.is_active = false;
            OracleValidatorInfo::insert(submission.validator_id, info);
        }
    }

    // Update aggregated price
    let aggregated = AggregatedPrice {
        asset_id,
        median_price: median,
        valid_submissions: submissions.len() as u32,
        timestamp: current_timestamp(),
        block_updated: current_block_height(),
    };
    AggregatedPrices::insert(asset_id, aggregated);

    // Append TWAP historical record
    PriceHistory::push(asset_id, HistoricalPrice {
        price: median,
        timestamp: current_timestamp(),
    });

    emit_event("PriceUpdated", asset_id, median, current_timestamp());
    Ok(())
}
```

### 25.4 EVM Precompiled Interface

DeFi contracts read price data through a precompiled contract:

```solidity
/// Precompiled contract address: 0x0000...0101
interface ICallOracle {
    /// Get latest price
    /// @return price denominated in CALL (18 decimals)
    /// @return timestamp of price update
    function getPrice(bytes32 assetId)
        external view
        returns (uint256 price, uint256 timestamp);

    /// Get time-weighted average price (TWAP)
    /// @param window time window (seconds)
    /// @return twap time-weighted average price
    function getTWAP(bytes32 assetId, uint256 window)
        external view
        returns (uint256 twap);

    /// Check if price is stale
    /// @param maxAge maximum allowed age (seconds)
    /// @return isStale true if price is stale
    function isStale(bytes32 assetId, uint256 maxAge)
        external view
        returns (bool isStale);

    /// Get oracle status
    /// @return updateInterval price update interval (blocks)
    /// @return quorum quorum count
    function getOracleStatus()
        external view
        returns (uint256 updateInterval, uint256 quorum);
}
```

**Usage Example:**

```solidity
contract MyDEX {
    ICallOracle public constant ORACLE = ICallOracle(0x0000000000000000000000000000000000000101);

    function swap(address tokenIn, uint256 amountIn) external {
        (uint256 price, uint256 timestamp) = ORACLE.getPrice(tokenIn);

        // Check price is valid
        require(!ORACLE.isStale(tokenIn, 300), "Price too stale");

        // Calculate output amount
        uint256 amountOut = (amountIn * price) / 1e18;

        // Execute swap...
    }
}
```

### 25.5 Configuration Parameters

| Parameter | Default Value | Description |
|------|--------|------|
| Update interval | 1000 blocks (~4 minutes) | Price update frequency |
| Quorum | 14 (2/3 of 21) | Minimum submissions to trigger aggregation |
| Outlier threshold | 5% | Deviation >5% from median marked as anomalous |
| Outlier tolerance limit | 10 times | 10 cumulative anomalies lose submission eligibility |
| TWAP window cap | 24 hours | TWAP maximum query range |
| Price staleness | 15 minutes | Considered stale beyond this time |
| Initial data source requirement | ≥2 independent sources | Validators must obtain prices from at least 2 APIs |

### 25.6 Supported Assets

| Asset | Data Sources | Priority |
|------|--------|--------|
| CALL/USD | CoinGecko, Binance | Highest (protocol native) |
| USDC/USD | CoinGecko, Binance, Coinbase | High |
| ETH/USD | CoinGecko, Binance, Coinbase | High |
| BTC/USD | CoinGecko, Binance | Medium |
| Other protocol assets | Added as needed | Low |

---

## 26. Key Design Decisions

### 26.1 Why Dual Ledger vs Single Ledger

| Single Ledger | Dual Ledger |
|----------|--------|
| EVM contracts need to adapt to protocol-layer API | EVM contracts completely independent, no adaptation needed |
| Complex implementation (lock-aware, state sync) | Simple implementation (two layers don't interfere) |
| DeFi contracts need modification | DeFi contracts are identical to Ethereum |
| Seamless user experience | Requires bridge operations (but can be automated) |

**Choice: Dual ledger**: Simplicity > seamless experience. Bridge operations can be automated through wallets for a near-seamless experience.

### 26.2 Why Commonware Simplex

- O(n) communication complexity, only 216 messages/round with 216 validators (vs ~46K for Tendermint)
- Simplest state machine -- round, proposal, vote, commit, four steps per round
- `commonware-consensus` crate ready to use, no need to implement from scratch
- Native subset rotation support -- random proposer sampling each round
- Already production-validated on Tempo chain
- Smallest audit surface -- least code, feasible for formal verification

### 26.3 Why Reth over Custom Execution Engine

- By 2026, Reth is the de facto standard for EVM execution
- Complete toolchain (Foundry, Hardhat compatible)
- No need to reimplement EVM semantics
- Reth's modular architecture allows deep customization

---

## Appendix A: Transaction Lifecycle

```
1. User initiates operation
   ├── Protocol operation → Sign EvmTx targeting precompile (0x101–0x209) → broadcast to mempool
   ├── EVM contract call → Sign EvmTx → broadcast to mempool
   └── Bridge operation → Auto-created by wallet → broadcast

2. Validators collect transactions
   └── EVM transactions (including precompile calls) → ordered by gas price

3. Validators pack block
   └── Execute EVM transactions → update EVM state (precompiles read/write protocol state via StorageRef)

4. Simplex consensus block production
   ├── Proposer packs → broadcasts proposal
   ├── Validators vote → 2/3 majority
   └── Final confirmation → block is immutable

5. Node synchronization
   ├── Receive new block
   ├── Verify signatures and state root
   └── Update local state
```

## Appendix B: Glossary

| Term | Definition |
|------|------|
| Protocol Payment Layer | Protocol payment layer, maintains native balance mappings |
| EVM Contract Layer | EVM smart contract layer, runs smart contracts |
| Internal Bridge | Internal bridge, asset conversion mechanism between two layers |
| Asset Registry | Asset registry, records all protocol assets |
| CompliancePolicy | Compliance policy, defines transfer restrictions for assets |
| Simplex | Low-latency BFT consensus algorithm, O(n) communication complexity |
| Commonware | Rust implementation framework for Simplex consensus |

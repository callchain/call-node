# Seismic Analysis

Seismic is an EVM blockchain with native on-chain privacy. This document analyzes how Seismic achieves privacy, its key characteristics, and what call-node can learn from it.

---

## How Seismic Achieves Privacy

Seismic's privacy is built on a **TEE (Trusted Execution Environment) + native EVM extension** architecture. The core mechanisms are as follows.

### 1. Shielded Solidity Types

Seismic forked the Solidity compiler and EVM, introducing encrypted storage types prefixed with `s*`:

```solidity
mapping(uint256 => sbytes) secrets; // Values are stored encrypted on-chain
suint256 secretIndex;               // The index itself is also encrypted
```

- Variables such as `suint256` / `sbytes` / `saddress` are **automatically encrypted** in EVM storage slots.
- Contract execution occurs within a TEE enclave, where decryption is transparent to the logic layer.
- Explicit casts are required to convert to public values (e.g., `uint256(secretIndex)`).

### 2. Shielded EVM (`seismic-revm`)

A fork of revm with native support at the virtual machine level:

- **Encrypted SLOAD/SSTORE**: Automatic encryption/decryption when reading/writing shielded storage slots.
- **Encrypted calldata**: Transaction inputs can be encrypted, preventing MEV and front-running.
- **Encrypted return values**: Shielded returns from `view` functions are automatically encrypted during transmission.

### 3. TEE Node Architecture

Validator nodes run inside Intel SGX / AMD SEV or similar TEEs:

- Encrypted state is only decrypted within the enclave.
- Consensus validates the **correctness of encrypted state transitions**.
- External observers (including node operators) cannot see the contract's shielded state.

### 4. Frontend SDK Ecosystem

- **`seismic-viem`**: Provides `createShieldedWalletClient`, `ShieldedContract`, `ShieldedPublicClient`.
- **`seismic-react`**: Provides `useShieldedWallet`, `useShieldedContract`.
- **Special call paths**: `tread` (transparent read), `twrite` (transparent write).

### 5. Full Foundry Toolchain

`sforge`, `sanvil`, `ssolc` -- a fully compatible privacy-aware development toolchain.

---

## Key Characteristics

| Characteristic | Description |
| --- | --- |
| **Native EVM compatibility** | Solidity developers only need to learn `s*` types; no new language required. |
| **Selective privacy** | The same contract can mix public (`uint256`) and private (`suint256`) state. |
| **Conditional revelation** | Contract logic determines when and who can see private data (e.g., only contributors can call `rob()`). |
| **Encrypted transactions** | Prevents MEV, front-running, and transaction content leakage. |
| **Programmable privacy** | Not chain-wide anonymity, but privacy boundaries controlled by the contract. |

---

## What Call-Node Can Learn

### 1. Frontend SDK Abstraction Layer (Most Direct)

Call-node's shielded precompile (`0x202`) currently requires developers to **manually construct ABI, manage notes, and generate ZK proofs**, which is a very high barrier to entry. We can learn from the `seismic-viem` model:

```typescript
// Inspired by seismic-viem's ShieldedWalletClient
const client = createShieldedPoolClient({
  rpcUrl,
  viewingKey, // Automatically manages note decryption
});

// Automatically handles note selection + proof generation + encryption
await client.deposit(assetId, amount);
await client.transfer(recipientViewingKey, amount);
```

Corresponding React hooks:

```typescript
// Inspired by useShieldedWallet / useShieldedContract
const { merkleRoot, notes, deposit, transfer } = useShieldedPool();
```

### 2. Selective Privacy Design Philosophy

Seismic's `ClownBeatdown` demonstrates best practices for **mixing public and private state in the same contract**:

- `clownStamina` (`uint256`) -- Public, visible to everyone.
- `secrets` (`sbytes`) -- Private, only revealed when conditions are met.
- `secretIndex` (`suint256`) -- Private, re-randomized on reset.

Call-node's shielded pool is currently an "all-or-nothing" UTXO model. We can allow EVM contracts to achieve similar effects through precompiles: **public state drives game logic, while private state protects sensitive data**.

### 3. Conditional Revelation Pattern

The access control in `rob()` is worth learning from:

```solidity
function rob() public view requireDown onlyContributor returns (bytes memory)
```

Call-node's shielded precompile can be extended with **conditional withdrawal**: instead of allowing withdrawal with just a proof, additional contract-level access controls can be enforced (e.g., only the game winner can withdraw rewards from the shielded pool).

### 4. Developer Toolchain Experience

The local development experience with `sanvil` for shielded contracts is very smooth. Call-node's devnet could add:

- **Local fast mode for shielded precompiles** (skip real ZK verification, only structural checks).
- Automated note generation/decryption tools.
- ABI sync scripts similar to `contract:deploy`.

### 5. Transaction Encryption / MEV Protection

Seismic supports encrypted calldata, which is a natural advantage of the TEE route. As a ZK-based project, call-node could consider introducing optional encryption at the **transaction broadcast layer** (e.g., threshold-encrypted mempool) to prevent MEV.

---

## Fundamental Differences Between the Two Approaches

| Dimension | Seismic (TEE) | Call-Node (ZK) |
| --- | --- | --- |
| **Trust assumption** | Trust hardware vendors (Intel / AMD) | Trust cryptography (Groth16) |
| **Privacy type** | Execution privacy (encrypted state) | Verification privacy (zero-knowledge proof) |
| **EVM intrusiveness** | Requires forking revm + solc | Standard EVM + precompile |
| **Performance** | Fast (hardware decryption) | Slow (ZK proof generation / verification) |
| **Decentralization** | Relies on TEE remote attestation | Pure cryptography, no hardware dependency |

**Conclusion**: Call-node should not (and cannot easily) copy Seismic's TEE route, but it can directly learn from Seismic in three areas: **frontend DX, selective privacy design, and developer toolchain**, significantly lowering the barrier to using the shielded pool.

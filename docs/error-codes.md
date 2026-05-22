# Callchain Error Codes Reference

This document catalogs all error types emitted by the Callchain protocol and node software. Use it to debug transaction failures, precompile reverts, RPC errors, and node log messages.

---

## RPC Error Codes

Callchain uses standard JSON-RPC error codes alongside application-specific codes in the `-32000` range.

### Standard JSON-RPC Codes

| Code | Name | Meaning |
|------|------|---------|
| `-32700` | Parse error | Invalid JSON received |
| `-32600` | Invalid Request | JSON is not a valid Request object |
| `-32601` | Method not found | Method does not exist |
| `-32602` | Invalid params | Invalid method parameters |
| `-32603` | Internal error | Transport/server-level internal error |

### Application-Specific Codes (`crates/rpc/src/handlers/helpers.rs`)

| Code | Name | When Returned |
|------|------|---------------|
| `-32000` | InternalError | Generic application internal error |
| `-32001` | ExecutionReverted | EVM execution reverted (e.g., precompile revert) |
| `-32002` | ResourceUnavailable | Lock poisoned or resource temporarily unavailable |
| `-32003` | DatabaseError | MDBX or persistence layer error |
| `-32004` | MethodNotAvailable | Method disabled in current build configuration |
| `-32005` | TransactionValidationFailed | Mempool rejected the transaction |
| `-32006` | FilterNotFound | Filter ID does not exist |
| `-32007` | LightClientVerificationFailed | Light client proof or sync committee verification failed |
| `-32010` | InvalidHex | Invalid hex / base16 decoding in params |

---

## Protocol Errors (`ProtocolError`)

`crates/protocol/src/lib.rs`

General-purpose protocol errors used across multiple modules.

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InsufficientBalance` | "insufficient balance" | Account balance too low for operation |
| `Unauthorized` | "unauthorized" | Caller lacks permission |
| `NonceError(s)` | "nonce error: {s}" | Invalid or out-of-order nonce |
| `Compliance(s)` | "compliance check failed: {s}" | Asset sanctions or issuer policy blocked |
| `GasError(s)` | "gas error: {s}" | Out of gas or invalid gas parameters |
| `BalanceError(s)` | "balance error: {s}" | Arithmetic overflow/underflow in balance logic |
| `SponsorError(s)` | "sponsor error: {s}" | Gas sponsorship failure |
| `AssetError(s)` | "asset error: {s}" | Generic asset operation failure |
| `RegistryError(s)` | "registry error: {s}" | Asset registry lookup failed |
| `ReceiptError(s)` | "receipt error: {s}" | Receipt storage/retrieval failure |
| `Economics(s)` | "economics error: {s}" | Fee or reward calculation failure |
| `Recovery(s)` | "recovery error: {s}" | Signature recovery failed |
| `SessionKey(s)` | "session key error: {s}" | Invalid or expired session key |
| `InvalidSignature(s)` | "invalid signature: {s}" | Bad cryptographic signature |

---

## Asset Errors (`AssetError`)

`crates/asset/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InsufficientBalance` | "insufficient balance" | Transfer amount exceeds holding |
| `BalanceOverflow` | "balance overflow" | Addition would exceed u128 |
| `SupplyOverflow` | "supply overflow" | Minting would overflow total supply |
| `SupplyUnderflow` | "supply underflow" | Burning would underflow total supply |
| `MaxSupplyExceeded` | "max supply exceeded" | Hard cap on asset supply reached |
| `NotIssuer` | "not asset issuer" | Only issuer can mint/burn/modify |
| `AssetNotFound` | "asset not found" | Unknown `asset_id` |
| `InsufficientAllowance` | "insufficient allowance" | ERC-20-style allowance too low |
| `ComplianceFailed` | "compliance check failed" | Sanctions list or policy rejection |
| `InvalidMetadata(key)` | "invalid metadata: {key}" | Bad metadata key in asset registration |
| `AssetIdOverflow` | "asset id overflow" | Next asset ID would overflow |
| `InvalidDecimals(d)` | "invalid decimals: {d}" | Decimals > 18 |
| `Erc20AlreadyBound(addr)` | "ERC-20 contract already bound: {addr}" | Duplicate ERC-20 wrapper registration |

---

## Bridge Errors (`BridgeError`)

`crates/bridge/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `Protocol(e)` | "protocol error: {e}" | Wrapped `ProtocolError` |
| `InsufficientProtocolBalance(asset, need)` | "insufficient protocol balance: asset={asset}, need={need}" | Bridge contract underfunded |
| `InsufficientEvmBalance(contract, need)` | "insufficient EVM balance: contract={contract}, need={need}" | EVM-side bridge balance too low |
| `AssetNotRegistered(asset)` | "asset not registered: {asset}" | Bridging unregistered asset |
| `BridgePaused(asset)` | "bridge paused for asset: {asset}" | Asset-specific bridge pause |
| `ExternalBridgePaused` | "external bridge globally paused" | Global bridge pause active |
| `ExceedsMaxPerTx(asset, amount, limit)` | "exceeds max per tx: asset={asset}, amount={amount}, limit={limit}" | Single-deposit cap exceeded |
| `ExceedsDailyLimit(asset, used, limit)` | "exceeds daily limit: asset={asset}, daily_used={used}, limit={limit}" | Daily deposit cap hit |
| `InsufficientSignatures(got, required)` | "insufficient bridge signatures: got={got}, required={required}" | Not enough validator attestations |
| `InvalidSignature(index, reason)` | "invalid bridge signature at index {index}: {reason}" | Bad validator signature |
| `SignatureTimeout(source_tx)` | "bridge signature timeout: source_tx={source_tx:?}" | Attestation timed out |
| `ExternalAssetNotAllowed(asset)` | "external bridge asset not allowed: {asset}" | Asset not in bridge allowlist |
| `UnauthorizedBridgeContract(chain, contract)` | "unauthorized bridge contract: chain={chain}, contract={contract}" | Wrong source chain contract |
| `BridgeFeeExceedsAmount(fee, amount)` | "bridge fee exceeds amount: fee={fee}, amount={amount}" | Fee larger than transfer amount |
| `BalanceOverflow` | "balance overflow" | Bridge balance arithmetic overflow |
| `EvmExecutionFailed(s)` | "EVM execution failed: {s}" | EVM revert during bridge operation |
| `MptProofError(s)` | "MPT proof verification failed: {s}" | Invalid Merkle Patricia proof |
| `NotConsensusVerified(block)` | "block {block} not consensus-verified by beacon chain" | Light client block verification failed |

---

## Governance Errors (`GovernanceError`)

`crates/governance/src/error.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `ProposalNotFound` | "proposal not found" | Invalid `proposal_id` |
| `InsufficientDeposit` | "insufficient deposit (need 10,000 CALL)" | Deposit below minimum |
| `VotingNotStarted` | "voting has not started yet" | Called `vote()` before voting opens |
| `VotingPeriodClosed` | "voting period has closed" | Called `vote()` after deadline |
| `NoVotingPower` | "voter has no voting power" | Caller has no delegated stake |
| `ProposalDefeated` | "proposal defeated" | Proposal failed quorum/threshold |
| `ProposalNotQueued` | "proposal not queued for execution" | `execute()` called before `queue()` |
| `TimelockNotElapsed` | "timelock period has not elapsed" | Executing before timelock expires |
| `VotingPeriodNotEnded` | "voting period has not ended" | Queuing before voting closes |
| `ExecutionTimeoutNotReached` | "execution timeout has not been reached" | Expired proposal not yet past timeout |
| `InsufficientBalanceForDelegation` | "insufficient balance for delegation" | Not enough CALL to delegate |
| `NoDelegation` | "no active delegation" | Undelegating without prior delegation |
| `NoValidators` | "no validators registered" | Empty validator set edge case |
| `ValidatorNotFound` | "validator not found" | Invalid validator reference |
| `NotPaused` | "chain is not paused" | `emergencyResume()` when not paused |
| `AlreadyVoted` | "voter has already voted on this proposal" | Double-voting attempt |
| `ExecutionFailed(s)` | "execution failed: {s}" | Proposal execution reverted |
| `ProposalRateLimited(remaining)` | "proposal rate limited — {remaining} blocks remaining" | Too many proposals from same proposer |
| `UnauthorizedExecutor` | "unauthorized executor" | Non-governance caller on protected operation |

---

## Validator Errors (`ValidatorError`)

`crates/validator/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InsufficientBalance` | "insufficient balance" | Not enough CALL to stake |
| `BalanceOverflow` | "balance overflow" | Stake addition overflow |
| `EscrowOverflow` | "escrow overflow" | Total staked overflow |
| `EscrowUnderflow` | "escrow underflow" | Unstake amount exceeds escrow |
| `BelowMinimumStake` | "below minimum self-stake" | Stake < 1,000,000 CALL |
| `AlreadyStaked` | "already staked" | Duplicate stake from same address |
| `NotAValidator` | "not a validator" | Operation on non-validator address |
| `IdMismatch` | "validator id mismatch" | Validator ID does not match address |
| `AlreadyUnbonding` | "already unbonding" | Second unstake while unbonding |
| `NotUnbonding` | "not unbonding" | Claiming without prior unstake |
| `UnbondingPeriodNotElapsed` | "unbonding period not elapsed" | Claiming before lock expires |
| `NoUnbondingRequest` | "no unbonding request found" | Missing unbonding record |
| `BelowSafetyFloor` | "below safety floor" | Would drop active validator count too low |
| `InvalidPubkey` | "invalid pubkey" | Bad Ed25519 consensus key |
| `ValidatorCountOverflow` | "validator count overflow" | Would exceed max validator set size |

---

## Oracle Errors (`OracleError`)

`crates/oracle/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `ValidatorNotFound` | "validator not found" | Submitter not in validator set |
| `ValidatorDisabled` | "validator disabled" | Validator deactivated |
| `WrongPeriod` | "wrong period" | Submission outside allowed window |
| `InvalidSignature` | "invalid signature" | Bad Ed25519 price signature |
| `InsufficientDataSources(got, need)` | "insufficient data sources: got {got}, need {need}" | Too few price sources |
| `DisallowedSource(source)` | "disallowed data source: {source}" | Source not in allowlist |

---

## Shielded Errors (`ShieldedError`)

`crates/shielded/src/precompile.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InsufficientBalance` | "insufficient balance" | Not enough funds for deposit/withdraw |
| `BalanceOverflow` | "balance overflow" | Balance arithmetic overflow |
| `MerkleRootMismatch` | "merkle root mismatch" | Proof references unknown Merkle root |
| `InvalidZkProof` | "invalid ZK proof" | Halo2 proof verification failed |
| `ZkProofError(s)` | "ZK verification error: {s}" | Detailed proof failure reason |
| `NullifierAlreadySpent` | "nullifier already spent" | Double-spend attempt |
| `EmptyBatch` | "empty batch" | Transfer with no inputs/outputs |
| `NoteExpired` | "note expired" | Note past expiry window |
| `MerkleTreeFull` | "merkle tree commitment cap reached" | Tree capacity exceeded |
| `Paused` | "shielded pool is paused" | Emergency pause active |
| `CommitmentAlreadyExists` | "commitment already exists" | Duplicate commitment insertion |
| `RateLimitExceeded` | "per-address rate limit exceeded" | Too many shielded ops from same address |
| `InvalidAmount` | "amount must be greater than zero" | Zero or negative amount |
| `Unauthorized` | "unauthorized caller" | Caller lacks permission |
| `PruneTooFrequent` | "prune called too frequently" | Rate limit on `pruneNullifiers` |

---

## Switch Errors (`SwitchError`)

`crates/switch/src/precompile.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `AssetNotActive` | "asset not active" | Asset disabled or not yet registered |
| `AssetHasNoErc20Bridge` | "asset has no ERC-20 bridge; use protocol-only operations" | Trying EVM switch on protocol-only asset |
| `EvmContractNotRegistered` | "EVM contract not registered for asset" | Missing ERC-20 wrapper |
| `InsufficientProtocolBalance` | "insufficient protocol balance" | Not enough protocol-layer balance |
| `InsufficientEvmBalance` | "insufficient EVM balance" | Not enough ERC-20 balance |
| `ProtocolBalanceOverflow` | "protocol balance overflow" | Addition overflow |
| `ProtocolBalanceUnderflow` | "protocol balance underflow" | Subtraction underflow |
| `AmountMustBePositive` | "amount must be > 0" | Zero or negative switch amount |
| `ToCannotBeZero` | "to cannot be zero address" | Zero-address destination |

---

## Compliance Errors (`ComplianceError`)

`crates/compliance/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `NotGovernance` | "not governance" | Only governance can modify compliance |
| `InvalidStatus` | "invalid compliance status value" | Status byte not 0 or 1 |

---

## Agent Errors (`AgentError`)

`crates/agent/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `NotFound` | "agent: not found" | Unknown `agent_id` |
| `NotOwner` | "agent: sender is not owner" | Unauthorized agent operation |
| `PermissionsExpired` | "agent: permissions expired" | Agent session expired |
| `AssetNotAllowed` | "agent: asset not allowed" | Agent restricted from asset |
| `AmountExceedsLimit` | "agent: amount exceeds per-tx limit" | Over agent spending cap |
| `InsufficientBalance` | "agent: insufficient balance" | Agent balance too low |
| `BalanceOverflow` | "agent: balance overflow" | Agent balance arithmetic overflow |
| `ArrayLengthMismatch` | "array length mismatch" | Batch param length mismatch |
| `EmptyBatch` | "empty batch" | Empty batch operation |
| `SessionNotFound` | "agent: session not found" | Invalid session key |
| `InvalidDelegate` | "agent: invalid delegate" | Unauthorized delegation |
| `AgentAlreadyExists` | "agent: address already registered" | Duplicate agent registration |

---

## Mempool Errors (`MempoolError`)

`crates/mempool/src/pool.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `Duplicate(hash)` | "duplicate transaction: {hash}" | Tx already in mempool |
| `PoolFull { pool, count, max }` | "pool capacity reached: {pool} full ({count}/{max})" | Mempool at capacity |
| `AddressLimitReached { address, count, max }` | "per-address limit reached: {address}, {count}/{max}" | Address spam limit |
| `FeeTooLow { fee, min }` | "fee too low: {fee} < {min}" | Below minimum fee for inclusion |
| `GasLimitExceeded { gas, max }` | "gas limit exceeded: {gas} > {max}" | Tx gas > block gas limit |
| `InvalidNonce { expected, got }` | "invalid nonce: expected {expected}, got {got}" | Wrong nonce (gap or replay) |
| `InsufficientBalance { required, got }` | "insufficient balance: required {required}, got {got}" | Can't afford gas |
| `NotFound(hash)` | "transaction not found: {hash}" | Tx not in mempool |
| `Generic(s)` | "mempool error: {s}" | Catch-all |

---

## Consensus Errors (`ConsensusError`)

`crates/consensus/src/validator.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InvalidBlock(s)` | "invalid block: {s}" | Block validation failure |
| `ProposerNotInSubset(id)` | "proposer not in subset: {id}" | Invalid proposer for round |
| `InvalidSignature` | "signature verification failed" | Bad block/round signature |
| `DoubleSign(id)` | "double sign detected: validator={id}" | Equivocation (slashing offense) |
| `ValidatorOffline(id)` | "validator offline: {id}" | Missed rounds threshold |
| `InsufficientStake` | "insufficient stake" | Stake below minimum |
| `UnbondingNotElapsed` | "unbonding period not elapsed" | Withdraw before lock expires |
| `ValidatorNotFound(id)` | "validator not found: {id}" | Unknown validator ID |
| `ConsensusError(s)` | "consensus error: {s}" | Catch-all |

---

## Storage Errors (`StorageError`)

`crates/storage/src/lib.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `MdbxError(e)` | "mdbx error: {e}" | Low-level MDBX failure |
| `KeyNotFound` | "key not found" | Missing database key |
| `SerializationError(e)` | "serialization error: {e}" | Serde encode/decode failure |
| `Corruption(s)` | "database corruption detected: {s}" | Inconsistent state |

---

## EVM Errors (`EvmError`)

`crates/evm/src/executor.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `ExecutionFailed(s)` | "EVM execution failed: {s}" | General revm failure |
| `InvalidTransaction(s)` | "invalid transaction: {s}" | Malformed tx or bad signature |
| `StateError(s)` | "state error: {s}" | State provider failure |
| `PrecompileError(s)` | "precompile error: {s}" | Protocol precompile revert |

---

## Cryptographic Errors

### `Secp256k1Error` (`crates/crypto/src/secp256k1.rs`)

| Variant | Message |
|---------|---------|
| `InvalidSignature` | "invalid signature" |
| `RecoveryFailed` | "recovery failed" |
| `VerificationFailed` | "signature verification failed" |

### `Ed25519Error` (`crates/crypto/src/ed25519.rs`)

| Variant | Message |
|---------|---------|
| `InvalidSignature` | "invalid Ed25519 signature" |
| `InvalidPublicKey` | "invalid Ed25519 public key" |
| `SigningFailed` | "signing failed" |

### `BlsError` (`crates/crypto/src/bls.rs`)

| Variant | Message |
|---------|---------|
| `InvalidSignature` | "invalid BLS signature" |
| `InvalidPublicKey` | "invalid BLS public key" |
| `AggregationFailed` | "BLS signature aggregation failed" |

### `SignerError` (`crates/crypto/src/signer.rs`)

| Variant | Message |
|---------|---------|
| `SigningFailed(s)` | "signing failed: {s}" |
| `KmsError(s)` | "KMS error: {s}" |

### `KeystoreError` (`crates/crypto/src/keystore.rs`)

| Variant | Message |
|---------|---------|
| `InvalidPassword` | "invalid keystore password" |
| `Io(s)` | "I/O error: {s}" |
| `Crypto(s)` | "crypto error: {s}" |

---

## Network Errors (`NetworkError`)

`crates/network/src/limits.rs`

| Variant | Message | Typical Cause |
|---------|---------|---------------|
| `InvalidLimits(s)` | "invalid limits config: {s}" | Bad network configuration |
| `MessageTooLarge { size, max }` | "message too large: {size} bytes (max {max})" | Oversized P2P message |
| `RateLimitExceeded` | "rate limit exceeded for peer" | Peer sending too fast |
| `PeerLimitReached { current, max }` | "peer limit reached: {current}/{max}" | Too many connected peers |
| `PeerBanned { reason }` | "peer banned: {reason}" | Previously misbehaving peer |
| `PeerNotFound { peer_id }` | "peer not found: {peer_id}" | Unknown peer ID |
| `NetworkError(s)` | "network error: {s}" | Catch-all |

---

## Light Client Errors (`LightClientError`)

`crates/light-client/src/types.rs`

| Variant | Message |
|---------|---------|
| `InvalidHeader(s)` | "invalid beacon header: {s}" |
| `SyncCommitteeSignatureInvalid(s)` | "sync committee signature invalid: {s}" |
| `SyncCommitteeUpdateFailed(s)` | "sync committee update failed: {s}" |
| `AlreadyInitialized` | "sync committee already initialized" |
| `InsufficientSyncParticipation { got, required }` | "insufficient sync committee participation: {got}/{required}" |

---

## Node Light Client Errors (`LightClientError`)

`crates/node/src/light_client.rs`

| Variant | Message |
|---------|---------|
| `ParentHashMismatch { expected, got }` | "parent hash mismatch: expected {expected}, got {got}" |
| `InsufficientSignatures { got, required }` | "insufficient signatures: got {got}, required {required}" |
| `StateRootMismatch { expected, got }` | "state root mismatch: expected {expected}, got {got}" |
| `InvalidHeader(s)` | "invalid header: {s}" |
| `InvalidZkProof` | "invalid ZK proof" |
| `InvalidNullifier` | "invalid nullifier" |
| `InvalidCommitment` | "invalid commitment" |
| `InvalidMerkleProof` | "invalid Merkle proof" |
| `CommitmentMismatch` | "commitment does not match proof leaf" |
| `ChainIdMismatch` | "chain ID mismatch" |
| `InvalidBlsAggregate(s)` | "invalid BLS aggregate signature: {s}" |

---

## MPT Verification Errors (`MptError`)

`crates/light-client/src/verifier.rs`

| Variant | Message |
|---------|---------|
| `InvalidRlp` | "invalid RLP encoding" |
| `ProofIncomplete` | "proof incomplete: missing nodes" |
| `NodeHashMismatch { expected, actual }` | "node hash mismatch: expected {expected}, got {actual}" |
| `InvalidNode` | "invalid node encoding" |

---

## Bridge Precompile Errors (`BridgeError`)

`crates/bridge/src/precompile.rs`

| Variant | Message |
|---------|---------|
| `AssetNotRegistered` | "asset not registered" |
| `BridgePaused` | "bridge paused" |
| `AssetZeroNotBridgeable` | "asset 0 not bridgeable" |
| `AlreadyProcessed` | "source tx already processed" |
| `NotProcessed` | "source tx not processed" |
| `AlreadyChallenged` | "challenge already exists" |
| `ChallengePeriodExpired` | "challenge period expired" |
| `ChallengeNotPending` | "challenge not pending" |
| `ChallengeDeadlineNotReached` | "challenge deadline not reached" |
| `NotChallenger` | "not challenger" |
| `ChallengeNotSuccessful` | "challenge not successful" |
| `ExceedsMaxPerTx(asset, amount, limit)` | "exceeds max per tx: asset={asset}, amount={amount}, limit={limit}" |
| `ExceedsDailyLimit(asset, used, limit)` | "exceeds daily limit: asset={asset}, daily_used={used}, limit={limit}" |
| `ExternalAssetNotAllowed(asset)` | "external bridge asset not allowed: {asset}" |
| `UnauthorizedBridgeContract(chain, contract)` | "unauthorized bridge contract: chain={chain}, contract={contract}" |
| `ExceedsWithdrawLimit(asset, withdrawn, limit)` | "exceeds withdraw limit: asset={asset}, period_withdrawn={withdrawn}, limit={limit}" |
| `BridgeFeeExceedsAmount(fee, amount)` | "bridge fee exceeds amount: fee={fee}, amount={amount}" |
| `UnauthorizedResolver(addr)` | "unauthorized resolver: {addr}" |

---

## Shielded Pool Errors (`ShieldedError`)

`crates/shielded/src/lib.rs`

| Variant | Message |
|---------|---------|
| `InvalidZkProof` | "invalid ZK proof" |
| `InvalidZkProofWithReason(s)` | "invalid ZK proof: {s}" |
| `DoubleSpend(nullifier)` | "double spend detected: nullifier {nullifier:?}" |
| `ValueViolation` | "shielded value conservation violated" |
| `NullifierNotFound(nullifier)` | "nullifier not found in set: {nullifier:?}" |
| `CommitmentNotFound(commitment)` | "note commitment not found: {commitment:?}" |
| `InvalidViewingKey` | "invalid viewing key" |
| `ComplianceViolation(s)` | "compliance violation: {s}" |
| `LimitExceeded(current, max)` | "per-block shielded limit exceeded: {current} > {max}" |

---

## Fork / Upgrade Errors (`ForkError`)

`crates/consensus/src/fork.rs`

| Variant | Message |
|---------|---------|
| `ValidatorNotFound(id)` | "validator not found: {id}" |
| `InvalidSignature` | "invalid signature" |
| `VersionMismatch { block_height, expected, actual }` | "version mismatch at height {block_height}: expected {expected:?}, got {actual:?}" |
| `TimelockViolation(current, minimum)` | "timelock violation: activation at {current} but minimum is {minimum}" |
| `DuplicateRollbackSignature` | "duplicate rollback signature" |
| `RollbackTargetMismatch` | "rollback target mismatch" |
| `NoActiveRollback` | "no active rollback in progress" |
| `RollbackNonceMismatch { expected, got }` | "rollback nonce mismatch: expected {expected}, got {got}" |
| `UpgradeNotScheduled(version)` | "upgrade not scheduled: {version:?}" |

---

## Genesis Errors (`GenesisError`)

`crates/chainspec/src/genesis.rs`

| Variant | Message |
|---------|---------|
| `InvalidJson(s)` | "invalid genesis JSON: {s}" |
| `InvalidAddress(s)` | "invalid address: {s}" |
| `InvalidPublicKey(s)` | "invalid public key: {s}" |
| `ExecutionFailed(s)` | "execution failed: {s}" |
| `ChainIdMismatch { expected, actual }` | "chain ID mismatch: expected {expected}, got {actual}" |

---

## Serialization Errors (`SerializationError`)

`crates/serialization/src/lib.rs`

| Variant | Message |
|---------|---------|
| `RlpEncode(s)` | "RLP encode failed: {s}" |
| `RlpDecode(s)` | "RLP decode failed: {s}" |
| `JsonEncode(s)` | "JSON encode failed: {s}" |
| `JsonDecode(s)` | "JSON decode failed: {s}" |

---

## How to Read Revert Reasons

When a precompile call reverts, the EVM receipt contains:

```json
{
  "status": "0x0",
  "revertReason": "insufficient balance"
}
```

1. Check the `revertReason` string against the tables above.
2. Identify the module (e.g., `AssetError::InsufficientBalance` → asset precompile `0x201`).
3. Cross-reference with the transaction parameters to find the root cause.

For EVM transactions targeting precompiles, the revert reason is the `Display` output of the corresponding Rust error enum variant.

---

## Quick Reference by Precompile Address

| Precompile | Address | Relevant Error Types |
|------------|---------|---------------------|
| Asset | `0x201` | `AssetError`, `ProtocolError` |
| Bridge | `0x103` | `BridgeError` |
| Governance | `0x203` | `GovernanceError` |
| Validator / Staking | `0x204` | `ValidatorError` |
| Oracle | `0x205` | `OracleError` |
| Switch | `0x207` | `SwitchError` |
| Shielded | `0x202` | `ShieldedError` |
| Compliance | `0x208` | `ComplianceError` |
| Agent | `0x209` | `AgentError` |

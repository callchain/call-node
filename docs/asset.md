# Asset System

## Overview

The Callchain asset system supports both protocol-native assets (registered on-chain via consensus) and their corresponding ERC-20 wrapped representations on the EVM layer. This document describes the asset registration flow, EVM bridge registration, and the invariant guarantees between protocol and EVM state.

## Asset Registration

Assets are registered by calling `register(string,string,uint8,uint128)` on the **Asset precompile at `0x201`**. This is a standard EVM transaction that can be sent from MetaMask, Solidity contracts, or any Ethereum-compatible wallet.

Registration executes atomically during block execution, is ordered relative to other transactions, and is subject to the same consensus and fee rules as any EVM transaction.

### Execution Flow

During block execution, the Asset precompile (`0x201`) processes `register` as follows:

1. **Fee collection**: Deduct `asset_registration_fee` (in CALL, asset_id = 1) from the sender's balance. The fee amount is read from the governance config stored in EVM storage at `GOVERNANCE_ADDRESS` (`0x203`).
2. **Asset allocation**: Register the asset in `AssetRegistry`:
   - Allocate a monotonically increasing `asset_id` (starting from 1).
   - Store metadata: `symbol`, `name`, `decimals`, `issuer = sender`, `protocol_supply = 0`, `evm_supply = 0`, `max_supply` (from tx), `status = Active`, `compliance_policy = 0`, `registered_at = current_block_height`.
3. **EVM wrapped token deployment** (if `register_evm_bridge` is enabled): The system deployer (`Address::repeat_byte(0xFF)`) deploys a `WrappedToken` contract on the EVM layer with:
   - `name = asset.name`
   - `symbol = asset.symbol`
   - `decimals = asset.decimals`
   - `bridge = system deployer address`
   - `issuer = sender`
   - `maxSupply = max_supply`
   - `assetId = allocated asset_id`
4. **EVM contract address binding**: Set `asset.evm_contract_address` to the deployed contract address. This binding is immutable once set.

All four steps are atomic. If any step fails, the transaction reverts and no partial state is committed.

## ERC-20 Asset Registration

For assets that already exist as ERC-20 tokens on the EVM layer, use `registerErc20(address)` on the Asset precompile (`0x201`). This binds an existing ERC-20 contract to a protocol `asset_id`, enabling bidirectional switching via the Switch precompile (`0x207`).

### Execution Flow

1. **Metadata read**: The precompile reads `name`, `symbol`, and `decimals` directly from the ERC-20 contract's EVM storage slots (supports OpenZeppelin v4 and v5 layouts).
2. **Asset allocation**: Allocate a monotonically increasing `asset_id`.
3. **Metadata storage**: Store the discovered `symbol`, `name`, `decimals`, `issuer = Address::ZERO` (no one can mint), `protocol_supply = 0`, `evm_supply = 0`, `max_supply = 0` (uncapped).
4. **ERC-20 binding**: Store `evm_contract_address` from the caller-provided address. This address is used by the Switch precompile for `switchToEvm` / `switchToProtocol`.
5. **Has-ERC-20 flag**: Set `has_erc20 = 1` in asset metadata. This flag is checked by the Switch precompile — only assets with `has_erc20 == 1` (or CALL, `asset_id == 1`) can be switched.

### Protocol-Only vs ERC-20 Backed

| Property | Protocol-Only (`register`) | ERC-20 Backed (`registerErc20`) |
|----------|---------------------------|--------------------------------|
| `has_erc20` | `0` | `1` |
| `evm_contract_address` | `None` | Set from caller argument |
| `issuer` | `msg.sender` | `Address::ZERO` (no one can mint) |
| `max_supply` | Caller-specified | `0` (uncapped) |
| `switchToEvm` | ❌ Rejected | ✅ Allowed |
| `switchToProtocol` | ❌ Rejected | ✅ Allowed |
| `mint` / `burn` | ✅ Allowed (issuer only) | ❌ Not allowed (issuer = ZERO) |
| `transfer` | ✅ Allowed | ✅ Allowed |

Protocol-only assets live entirely within the Callchain protocol layer and cannot interact with EVM contracts. ERC-20 backed assets bridge between protocol and EVM via the Switch precompile.

### Why an EVM Transaction Instead of Direct RPC

Direct RPC state mutations (writing to EVM storage without an EVM transaction) would have the following issues:

- **No consensus ordering**: Each node would process the registration independently, leading to potential state divergence under concurrent registrations.
- **No atomic rollback**: If the RPC handler crashed or was interrupted, the registry could be left in an inconsistent state.
- **No fee/nonce enforcement**: The registration would bypass the standard transaction validation pipeline.

Using an EVM transaction to the Asset precompile (`0x201`) solves all of these by leveraging the existing EVM execution and block-building infrastructure.

### RPC Interface

`call_submit` with `type: "RegisterAsset"` is the user-facing convenience RPC (historical reference — the `call_submit` endpoint has been removed; use `eth_sendRawTransaction` with the Asset precompile directly):

```json
POST /{
  "jsonrpc": "2.0",
  "method": "call_submit",
  "params": {
    "sender": "0x...",
    "nonce": 42,
    "instructions": [
      {
        "type": "RegisterAsset",
        "symbol": "MYTOKEN",
        "name": "My Token",
        "decimals": 18,
        "max_supply": "1000000000000000000000000"
      }
    ],
    "signature": "0x..."
  },
  "id": 1
}
```

The RPC handler:
1. Parses parameters and validates the EIP-191 signature.
2. Maps the precompile call to an EVM transaction targeting `0x201` with the `register` selector.
3. Submits the EVM transaction via `eth_sendRawTransaction` → mempool.
4. Returns the pending `txHash`.

The actual registration is executed when the EVM transaction is included in a block and the precompile is invoked by revm.

## Asset Metadata

```rust
pub struct Asset {
    pub id: AssetId,                        // protocol asset identifier
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,                    // who registered the asset
    pub protocol_supply: Balance,           // EVM-tracked protocol-layer circulation
    pub evm_supply: Balance,                // EVM wrapped token circulation
    pub max_supply: Balance,                // 0 = uncapped
    pub status: AssetStatus,                // Active | Frozen | Delisted
    pub compliance_policy: u8,
    pub registered_at: u64,
    pub has_erc20: bool,                    // true if asset has an ERC-20 bridge
    pub evm_contract_address: Option<Address>, // set at registration time, immutable
}
```

| Field | Meaning | Source of truth |
|-------|---------|-----------------|
| `protocol_supply` | Total asset balance held in protocol accounts (tracked in EVM storage) | EVM storage under `0x201` |
| `evm_supply` | Total wrapped ERC-20 supply on EVM (`WrappedToken.totalSupply`) | EVM contract |
| `all_supply()` | Entire system circulation = protocol + EVM | Computed |
| `max_supply` | Hard cap (0 = uncapped). Set at registration, immutable. | Registration tx |

### Supply Cap Enforcement

The cap is checked **exactly once in protocol `Block::execute`**, never in the EVM contract. The EVM contract only sees `evm_supply`; it cannot know `protocol_supply`. Checking the cap in the contract would allow bypass (e.g., mint 8000 on protocol, then mint 3000 on EVM — contract sees 0 + 3000 <= 10000 and passes, but real `all_supply = 11000`).

```rust
let asset = registry.get_asset(asset_id).unwrap();
if asset.would_exceed_cap(amount) {
    return Err(ConsensusError::InvalidBlock("max supply exceeded".into()));
}
```

The EVM `WrappedToken` contract does **not** enforce the cap — the asset precompile is the single source of truth.

## Bridge Invariants

For any asset with an active EVM bridge:

```
sum(wrapped tokens burned via switchToEvm)
  == sum(wrapped tokens minted via switchToProtocol)
  == evm_wrapped_token.totalSupply()
```

This invariant is maintained by the bridge precompile (`0x207`):

- `switchToEvm`: Burns the caller's wrapped ERC-20 tokens (via `bridgeBurn`), then credits the corresponding EVM native balance.
- `switchToProtocol`: Deducts EVM native balance, then mints wrapped ERC-20 tokens (via `bridgeMint`) to the recipient.

There is no separate protocol-layer ledger. All balances — both wrapped token holdings and "protocol" native balances — are tracked in EVM storage.

## EVM Wrapped Token Reference Template

The system provides a reference `WrappedToken.sol` contract that satisfies the bridge requirements:

```solidity
contract WrappedToken {
    string public name;
    string public symbol;
    uint8 public decimals;
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    address public bridge;
    address public issuer;
    uint256 private _maxSupply;  // 0 = uncapped
    uint256 public assetId;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(
        string memory _name,
        string memory _symbol,
        uint8 _decimals,
        address _bridge,
        address _issuer,
        uint256 maxSupply_,
        uint256 _assetId
    ) {
        name = _name;
        symbol = _symbol;
        decimals = _decimals;
        totalSupply = 0;
        bridge = _bridge;
        issuer = _issuer;
        _maxSupply = maxSupply_;
        assetId = _assetId;
    }

    function cap() public view returns (uint256) { return _maxSupply; }

    function transfer(address _to, uint256 _value) public returns (bool) { /* ... */ }
    function approve(address _spender, uint256 _value) public returns (bool) { /* ... */ }
    function transferFrom(address _from, address _to, uint256 _value) public returns (bool) { /* ... */ }

    function bridgeMint(address _to, uint256 _value) public {
        require(msg.sender == bridge, "only bridge");
        totalSupply += _value;
        balanceOf[_to] += _value;
        emit Transfer(address(0), _to, _value);
    }

    function bridgeBurn(uint256 _value) public {
        require(balanceOf[msg.sender] >= _value, "insufficient balance");
        totalSupply -= _value;
        balanceOf[msg.sender] -= _value;
        emit Transfer(msg.sender, address(0), _value);
    }

    function issuerMint(address _to, uint256 _value) public {
        require(msg.sender == issuer, "only issuer");
        totalSupply += _value;
        balanceOf[_to] += _value;
        emit Transfer(address(0), _to, _value);
    }
}
```

Note: The `bridge` address is passed as a constructor argument and must match the protocol bridge address (`Address::repeat_byte(0xFF)`) used in `SwitchToEvm`. The contract does **not** enforce `max_supply` — cap checks happen in protocol `Block::execute` where both `protocol_supply` and `evm_supply` are visible.

## EVM Issuer Mint

Asset issuers can mint wrapped ERC-20 tokens directly on the EVM layer via the Asset precompile (`0x201`) `mint` function:

```solidity
function mint(uint64 assetId, address to, uint128 amount) external returns (bool);
```

### Execution Flow

1. **Issuer verification**: `msg.sender == asset.issuer`.
2. **Asset status check**: `asset.status == AssetStatus::Active`.
3. **Cap check**: `asset.all_supply() + amount <= max_supply` (protocol layer).
4. **EVM call**: The precompile calls `evm_executor.evm_call_issuer_mint(issuer, contract_addr, evm_state, to, amount)`.
5. **Supply tracking**: `registry.add_evm_supply(asset_id, amount)`.

No protocol-layer balance is created. The minted tokens exist only on EVM and can be withdrawn back to protocol via `switchToProtocol` (`0x207`).

### Security Properties

- Only the asset issuer can call `mint`.
- The cap is enforced at the protocol layer, not in the EVM contract (the contract has no visibility into `protocol_supply`).
- Genesis assets (e.g., CALL, asset_id = 1) use `issuer = Address::ZERO`, which has no private key. Therefore, no one can mint genesis assets. Supply changes for genesis assets happen only through validator rewards in EVM storage.

## Governance Parameters

The following parameters affect asset registration and can be updated via governance:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `asset_registration_fee` | `1_000_000` CALL | Fee paid by the issuer to register an asset |
| `register_evm_bridge` | `true` | Whether to auto-deploy a wrapped token on EVM during registration |

## Delisting and Lifecycle

Assets can transition through the following states:

| Status | `Mint` (issuer) | `Burn` (issuer) | `Transfer` | `SwitchToEvm` | `SwitchToProtocol` | `EvmIssuerMint` |
|--------|-----------------|-----------------|------------|---------------|--------------------|-----------------|
| `Active` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `Frozen` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| `Delisted` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |

- **Active**: All operations permitted.
- **Frozen**: All user-facing operations are blocked. Can be unfrozen by the issuer via the Asset precompile.
- **Delisted**: Permanently retired by the issuer. Cannot be unfrozen.

State transitions are controlled by the asset issuer through precompile calls:
- `freezeAsset(assetId)` — caller must be issuer; reversible.
- `unfreezeAsset(assetId)` — caller must be issuer; only works on `Frozen` assets.
- `delistAsset(assetId)` — caller must be issuer; permanent.

Governance can also pause the entire chain via `GovernanceEmergencyPause`.

## AssetRegistry Persistence

`AssetRegistry` state lives in **EVM storage** under the Asset precompile address (`0x201`). There is no separate protocol-layer database table for asset metadata. All registry fields — including `protocol_supply`, `evm_supply`, asset metadata, and status — are stored as EVM storage slots and committed atomically with the EVM state root.

On node startup, the registry is loaded directly from EVM storage via `StorageRef`. There is no replay reconstruction from block history.

### EVM Contract Address Consistency

`RegisterAsset` auto-deploys a wrapped ERC-20 via `deploy_erc20_template`. The contract address depends on the deployer nonce. The deployed contract code and state are part of the EVM state and are loaded from the EVM state root on startup.

## Genesis Asset Supply Initialization

Genesis assets (e.g., CALL, asset_id = 1) use `issuer = Address::ZERO`. This is an intentional security design:

- **No one can issuer-mint**: `Address::ZERO` has no corresponding private key, so `mint`, `burn`, and `issuerMint` precompile calls are all impossible for genesis assets.
- **Supply changes only via system path**: Block rewards, validator incentives, and other protocol-level issuance update validator balances directly in EVM storage, not through user-signed precompile calls.
- **Protocol-controlled monetary policy**: The chain itself controls how much CALL enters circulation.

After genesis block execution, `protocol_supply` for CALL must be initialized to the total distributed amount. Subsequent block rewards update it via validator reward paths in EVM storage.

Genesis assets should use `max_supply = 0` (uncapped) because protocol-level issuance has its own economic rules.

## Asset System Roadmap

### Completed

| Item | Status |
|------|--------|
| Supply redesign (`protocol_supply`, `evm_supply`, `max_supply`, `all_supply()`) | ✅ Done |
| Supply cap enforcement in `Block::execute` | ✅ Done |
| EVM issuer mint (`issuerMint` precompile + `WrappedToken.issuerMint`) | ✅ Done |
| Frozen/Delisted asset enforcement in transaction execution | ✅ Done |
| AssetRegistry in EVM storage under `0x201` | ✅ Done |
| `RegisterEvmBridge` precompile removal | ✅ Done |
| `WithdrawFromEvm` → `SwitchToProtocol` rename | ✅ Done |
| `call_assetInfo` extended with supply fields | ✅ Done |
| Unified write via `eth_sendRawTransaction` to precompiles | ✅ Done |
| Fixed bridge address for `bridgeMint` | ✅ Done |

### Remaining

| Item | Status | Notes |
|------|--------|-------|
| WrappedToken runtime bytecode | ⚠️ Partial | `.bin` is valid and deployed correctly; `.bin-runtime` is empty but unused at runtime. Regenerate if needed for external verification. |
| `call_totalBalance` RPC | ⚠️ Legacy | Returns `Asset.total_supply` which no longer exists; should return `all_supply()` or be deprecated in favor of `call_assetInfo`. |

## Precompile Equivalents

All asset operations are also available via the **Asset precompile (`0x201`)**:

| Operation | Precompile Function | Address |
|---|---|---|
| `Transfer` | `transfer(uint64,address,uint128)` | `0x201` |
| `BatchTransfer` | `batchTransfer(uint64,address[],uint128[])` | `0x201` |
| `Approve` | `approve(uint64,address,uint128)` | `0x201` |
| `TransferFrom` | `transferFrom(uint64,address,address,uint128)` | `0x201` |
| `Mint` | `mint(uint64,address,uint128)` | `0x201` |
| `Burn` | `burn(uint64,address,uint128)` | `0x201` |
| `RegisterAsset` | `register(string,string,uint8,uint128)` | `0x201` |
| `RegisterErc20` | `registerErc20(address)` | `0x201` |

See [precompile.md](precompile.md) for the full ABI.

## Related Documents

- [switch.md](switch.md) — SwitchToEvm and SwitchToProtocol execution details
- [protocol.md](protocol.md) — Precompile execution and atomicity guarantees
- [transaction.md](transaction.md) — Transaction format, fees, and nonce rules
- [rpc.md](rpc.md) — RPC method reference

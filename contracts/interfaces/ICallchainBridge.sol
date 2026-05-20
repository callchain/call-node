// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title ICallchainBridge
/// @notice Interface for the Ethereum-side Callchain bridge contract.
///
/// Deposit flow (Ethereum -> Callchain):
///   1. User calls deposit() to lock tokens/ETH
///   2. Contract emits BridgeDeposit event
///   3. Callchain validators observe event and relay via externalDeposit (0x103)
///
/// Withdrawal flow (Callchain -> Ethereum):
///   1. User calls externalWithdraw() on Callchain (0x103)
///   2. Callchain emits ExternalWithdraw event
///   3. Validators observe event and call release() on this contract
///   4. Contract transfers locked tokens/ETH to recipient
interface ICallchainBridge {
    /// @notice Emitted when a user deposits assets into the bridge (Ethereum -> Callchain)
    /// @param depositId Unique identifier for this deposit (used for replay protection on Callchain)
    /// @param recipient Address on Callchain that will receive the deposited balance
    /// @param assetId Callchain asset identifier (0 = native ETH)
    /// @param amount Amount deposited (after any fees)
    /// @param nonce Monotonic deposit nonce
    event BridgeDeposit(
        bytes32 indexed depositId,
        address indexed recipient,
        uint64 assetId,
        uint128 amount,
        uint256 nonce
    );

    /// @notice Emitted when validators release assets (Callchain -> Ethereum)
    /// @param withdrawId Unique identifier for this withdrawal (sourceTxHash from Callchain)
    /// @param recipient Ethereum address receiving the assets
    /// @param assetId Callchain asset identifier
    /// @param amount Amount released
    event BridgeRelease(
        bytes32 indexed withdrawId,
        address indexed recipient,
        uint64 assetId,
        uint128 amount
    );

    /// @notice Emitted when an asset token mapping is updated
    /// @param assetId Callchain asset identifier
    /// @param token Ethereum token address (address(0) for native ETH)
    event AssetTokenSet(uint64 indexed assetId, address token);

    /// @notice Emitted when deposit limits are updated
    /// @param maxPerTx Maximum amount per single deposit transaction
    /// @param dailyLimit Maximum total deposits per asset per day
    event LimitsUpdated(uint128 maxPerTx, uint128 dailyLimit);

    /// @notice Deposit ERC-20 tokens to be bridged to Callchain
    /// @param recipient Address on Callchain that will receive the balance
    /// @param assetId Callchain asset identifier (must be mapped to an ERC-20)
    /// @param amount Amount of tokens to deposit
    /// @return depositId Unique identifier for this deposit
    function deposit(
        address recipient,
        uint64 assetId,
        uint128 amount
    ) external returns (bytes32 depositId);

    /// @notice Deposit native ETH to be bridged to Callchain
    /// @param recipient Address on Callchain that will receive the balance
    /// @return depositId Unique identifier for this deposit
    function depositEth(address recipient) external payable returns (bytes32 depositId);

    /// @notice Release locked assets to a recipient (validator-only)
    /// @param withdrawId Unique withdrawal identifier from Callchain (prevents replay)
    /// @param recipient Ethereum address to receive the assets
    /// @param assetId Callchain asset identifier
    /// @param amount Amount to release
    function release(
        bytes32 withdrawId,
        address recipient,
        uint64 assetId,
        uint128 amount
    ) external;

    /// @notice Map a Callchain assetId to an Ethereum token address
    /// @param assetId Callchain asset identifier
    /// @param token Ethereum token address (address(0) for native ETH)
    function setAssetToken(uint64 assetId, address token) external;

    /// @notice Update deposit limits
    /// @param _maxPerTx Maximum amount per single deposit
    /// @param _dailyLimit Maximum total deposits per asset per day
    function setLimits(uint128 _maxPerTx, uint128 _dailyLimit) external;

    /// @notice Pause deposits and releases
    function pause() external;

    /// @notice Unpause deposits and releases
    function unpause() external;

    /// @notice Recover accidentally sent ERC-20 tokens (admin-only, does not touch bridge reserves)
    /// @param token Token address to recover
    /// @param to Recipient address
    /// @param amount Amount to recover
    function rescueTokens(address token, address to, uint256 amount) external;

    /// @notice Check if a deposit has been processed
    /// @param depositId The deposit identifier
    /// @return True if processed
    function isDepositProcessed(bytes32 depositId) external view returns (bool);

    /// @notice Check if a withdrawal has been processed
    /// @param withdrawId The withdrawal identifier
    /// @return True if processed
    function isWithdrawalProcessed(bytes32 withdrawId) external view returns (bool);

    /// @notice Get the Ethereum token address for a Callchain assetId
    /// @param assetId Callchain asset identifier
    /// @return Ethereum token address (address(0) for native ETH)
    function assetToken(uint64 assetId) external view returns (address);
}

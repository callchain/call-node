// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {ICallchainBridge} from "./interfaces/ICallchainBridge.sol";

/// @title CallchainBridge
/// @notice Ethereum-side bridge contract for Callchain cross-chain asset transfers.
///
/// This contract holds assets deposited from Ethereum users and releases them
/// when authorized by Callchain validators. It is the counterpart to the
/// Bridge precompile (0x103) on Callchain.
///
/// --- Deposit Flow (Ethereum -> Callchain) ---
/// 1. User calls deposit() / depositEth() to lock tokens/ETH in this contract
/// 2. Contract emits BridgeDeposit(depositId, recipient, assetId, amount, nonce)
/// 3. Callchain validators monitor Ethereum logs for BridgeDeposit events
/// 4. Validators aggregate signatures and submit externalDeposit(0x103) on Callchain
/// 5. Callchain credits recipient's protocol balance (subject to challenge period)
///
/// --- Withdrawal Flow (Callchain -> Ethereum) ---
/// 1. User calls externalWithdraw() on Callchain (0x103)
/// 2. Callchain burns user's protocol balance, emits ExternalWithdraw event
/// 3. Validators monitor Callchain for ExternalWithdraw events
/// 4. Validators call release() on this contract to unlock assets
/// 5. Assets transferred to recipient on Ethereum
///
/// @dev The contract uses a monotonic nonce for deposit IDs to ensure uniqueness
///      and prevent replay attacks on the Callchain side.
contract CallchainBridge is ICallchainBridge {
    // ── Errors ─────────────────────────────────────────────────────────

    error NotAdmin();
    error NotValidator();
    error NotPaused();
    error AlreadyPaused();
    error AssetNotSupported(uint64 assetId);
    error AssetAlreadySet(uint64 assetId);
    error InvalidRecipient();
    error InvalidAmount();
    error ExceedsMaxPerTx(uint128 amount, uint128 limit);
    error ExceedsDailyLimit(uint64 assetId, uint128 requested, uint128 remaining);
    error DepositAlreadyProcessed(bytes32 depositId);
    error WithdrawalAlreadyProcessed(bytes32 withdrawId);
    error InsufficientBridgeBalance(uint64 assetId, uint128 requested, uint128 available);
    error EthTransferFailed(address recipient);
    error ZeroAddress();
    error RescueAssetMismatch();

    // ── Roles ──────────────────────────────────────────────────────────

    /// @notice Admin role: can set asset tokens, limits, pause, rescue tokens
    address public admin;

    /// @notice Validator role: can call release() to process withdrawals
    mapping(address => bool) public isValidator;

    /// @notice List of all validators for enumeration
    address[] public validators;

    // ── Asset Configuration ────────────────────────────────────────────

    /// @notice Mapping from Callchain assetId to Ethereum token address.
    /// address(0) represents native ETH.
    mapping(uint64 => address) public assetTokens;

    /// @notice Whether an asset has been configured
    mapping(uint64 => bool) public isAssetSupported;

    // ── Deposit Tracking ───────────────────────────────────────────────

    /// @notice Monotonic nonce for deposits
    uint256 public depositNonce;

    /// @notice Whether a deposit has been processed (prevents replay)
    mapping(bytes32 => bool) public isDepositProcessed;

    /// @notice Total amount deposited per asset (for accounting / monitoring)
    mapping(uint64 => uint128) public totalDeposited;

    // ── Withdrawal Tracking ────────────────────────────────────────────

    /// @notice Whether a withdrawal has been processed (prevents double-release)
    mapping(bytes32 => bool) public isWithdrawalProcessed;

    /// @notice Total amount released per asset (for accounting / monitoring)
    mapping(uint64 => uint128) public totalReleased;

    // ── Rate Limiting ──────────────────────────────────────────────────

    /// @notice Maximum deposit amount per single transaction
    uint128 public maxPerTx;

    /// @notice Maximum total deposits per asset per day
    uint128 public dailyLimit;

    /// @notice Amount deposited per asset in the current day
    mapping(uint64 => uint128) public dailyDeposited;

    /// @notice Block number when the daily limit was last reset (per asset)
    mapping(uint64 => uint256) public lastDepositDay;

    /// @notice Approximate Ethereum blocks per day (~7200 at 12s/block)
    uint256 public constant BLOCKS_PER_DAY = 7200;

    // ── Pause ──────────────────────────────────────────────────────────

    /// @notice Whether deposits and releases are paused
    bool public paused;

    // ── Modifiers ──────────────────────────────────────────────────────

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    modifier onlyValidator() {
        if (!isValidator[msg.sender]) revert NotValidator();
        _;
    }

    modifier whenNotPaused() {
        if (paused) revert AlreadyPaused();
        _;
    }

    // ── Constructor ────────────────────────────────────────────────────

    /// @notice Initialize the bridge contract
    /// @param _admin Admin address with governance rights
    /// @param _validators Initial validator addresses authorized to release
    /// @param _maxPerTx Maximum deposit amount per transaction
    /// @param _dailyLimit Maximum total deposits per asset per day
    constructor(
        address _admin,
        address[] memory _validators,
        uint128 _maxPerTx,
        uint128 _dailyLimit
    ) {
        if (_admin == address(0)) revert ZeroAddress();
        admin = _admin;

        for (uint256 i = 0; i < _validators.length; i++) {
            address v = _validators[i];
            if (v == address(0)) revert ZeroAddress();
            if (!isValidator[v]) {
                isValidator[v] = true;
                validators.push(v);
            }
        }

        maxPerTx = _maxPerTx;
        dailyLimit = _dailyLimit;
    }

    // ── Deposit Functions ──────────────────────────────────────────────

    /// @inheritdoc ICallchainBridge
    function deposit(
        address recipient,
        uint64 assetId,
        uint128 amount
    ) external whenNotPaused returns (bytes32 depositId) {
        if (recipient == address(0)) revert InvalidRecipient();
        if (amount == 0) revert InvalidAmount();
        if (!isAssetSupported[assetId]) revert AssetNotSupported(assetId);

        address token = assetTokens[assetId];
        if (token == address(0)) revert AssetNotSupported(assetId);

        _checkLimits(assetId, amount);

        // Pull tokens from sender
        _safeTransferFrom(token, msg.sender, address(this), amount);

        // Generate unique deposit ID
        depositNonce++;
        depositId = keccak256(
            abi.encodePacked(
                block.chainid,
                address(this),
                msg.sender,
                recipient,
                assetId,
                amount,
                depositNonce
            )
        );

        if (isDepositProcessed[depositId]) revert DepositAlreadyProcessed(depositId);
        isDepositProcessed[depositId] = true;

        totalDeposited[assetId] += amount;

        emit BridgeDeposit(depositId, recipient, assetId, amount, depositNonce);

        return depositId;
    }

    /// @inheritdoc ICallchainBridge
    function depositEth(
        address recipient
    ) external payable whenNotPaused returns (bytes32 depositId) {
        if (recipient == address(0)) revert InvalidRecipient();
        if (msg.value == 0) revert InvalidAmount();
        if (!isAssetSupported[0]) revert AssetNotSupported(0);

        uint128 amount = uint128(msg.value);

        _checkLimits(0, amount);

        // Generate unique deposit ID
        depositNonce++;
        depositId = keccak256(
            abi.encodePacked(
                block.chainid,
                address(this),
                msg.sender,
                recipient,
                uint64(0), // assetId for ETH
                amount,
                depositNonce
            )
        );

        if (isDepositProcessed[depositId]) revert DepositAlreadyProcessed(depositId);
        isDepositProcessed[depositId] = true;

        totalDeposited[0] += amount;

        emit BridgeDeposit(depositId, recipient, 0, amount, depositNonce);

        return depositId;
    }

    // ── Withdrawal Function ────────────────────────────────────────────

    /// @inheritdoc ICallchainBridge
    function release(
        bytes32 withdrawId,
        address recipient,
        uint64 assetId,
        uint128 amount
    ) external onlyValidator whenNotPaused {
        if (recipient == address(0)) revert InvalidRecipient();
        if (amount == 0) revert InvalidAmount();
        if (!isAssetSupported[assetId]) revert AssetNotSupported(assetId);

        if (isWithdrawalProcessed[withdrawId]) revert WithdrawalAlreadyProcessed(withdrawId);
        isWithdrawalProcessed[withdrawId] = true;

        address token = assetTokens[assetId];

        if (token == address(0)) {
            // Native ETH
            if (amount > address(this).balance) {
                revert InsufficientBridgeBalance(assetId, amount, uint128(address(this).balance));
            }
            (bool success, ) = recipient.call{value: amount}("");
            if (!success) revert EthTransferFailed(recipient);
        } else {
            // ERC-20
            uint256 balance = IERC20(token).balanceOf(address(this));
            if (amount > balance) {
                revert InsufficientBridgeBalance(assetId, amount, uint128(balance));
            }
            _safeTransfer(token, recipient, amount);
        }

        totalReleased[assetId] += amount;

        emit BridgeRelease(withdrawId, recipient, assetId, amount);
    }

    // ── Admin Functions ────────────────────────────────────────────────

    /// @inheritdoc ICallchainBridge
    function setAssetToken(uint64 assetId, address token) external onlyAdmin {
        if (isAssetSupported[assetId]) revert AssetAlreadySet(assetId);
        assetTokens[assetId] = token;
        isAssetSupported[assetId] = true;
        emit AssetTokenSet(assetId, token);
    }

    /// @notice Update the token address for an existing asset (emergency only)
    /// @param assetId Callchain asset identifier
    /// @param token New Ethereum token address
    function updateAssetToken(uint64 assetId, address token) external onlyAdmin {
        if (!isAssetSupported[assetId]) revert AssetNotSupported(assetId);
        assetTokens[assetId] = token;
        emit AssetTokenSet(assetId, token);
    }

    /// @inheritdoc ICallchainBridge
    function setLimits(uint128 _maxPerTx, uint128 _dailyLimit) external onlyAdmin {
        maxPerTx = _maxPerTx;
        dailyLimit = _dailyLimit;
        emit LimitsUpdated(_maxPerTx, _dailyLimit);
    }

    /// @notice Add a validator
    /// @param validator Address to add
    function addValidator(address validator) external onlyAdmin {
        if (validator == address(0)) revert ZeroAddress();
        if (isValidator[validator]) return;
        isValidator[validator] = true;
        validators.push(validator);
    }

    /// @notice Remove a validator
    /// @param validator Address to remove
    function removeValidator(address validator) external onlyAdmin {
        if (!isValidator[validator]) return;
        isValidator[validator] = false;

        // Remove from validators array
        for (uint256 i = 0; i < validators.length; i++) {
            if (validators[i] == validator) {
                validators[i] = validators[validators.length - 1];
                validators.pop();
                break;
            }
        }
    }

    /// @notice Transfer admin rights
    /// @param newAdmin New admin address
    function transferAdmin(address newAdmin) external onlyAdmin {
        if (newAdmin == address(0)) revert ZeroAddress();
        admin = newAdmin;
    }

    /// @inheritdoc ICallchainBridge
    function pause() external onlyAdmin {
        paused = true;
    }

    /// @inheritdoc ICallchainBridge
    function unpause() external onlyAdmin {
        paused = false;
    }

    /// @inheritdoc ICallchainBridge
    function rescueTokens(address token, address to, uint256 amount) external onlyAdmin {
        if (to == address(0)) revert ZeroAddress();

        // Prevent rescuing bridge reserve tokens
        for (uint64 i = 0; i < type(uint64).max; i++) {
            if (!isAssetSupported[i]) continue;
            if (assetTokens[i] == token) revert RescueAssetMismatch();
            // Break after reasonable iteration to avoid gas issues
            if (i > 1000) break;
        }

        if (token == address(0)) {
            (bool success, ) = to.call{value: amount}("");
            if (!success) revert EthTransferFailed(to);
        } else {
            _safeTransfer(token, to, amount);
        }
    }

    // ── View Functions ─────────────────────────────────────────────────

    /// @inheritdoc ICallchainBridge
    function assetToken(uint64 assetId) external view returns (address) {
        return assetTokens[assetId];
    }

    /// @notice Get the number of validators
    function validatorCount() external view returns (uint256) {
        return validators.length;
    }

    /// @notice Get all validator addresses
    function getValidators() external view returns (address[] memory) {
        return validators;
    }

    /// @notice Get the remaining daily deposit limit for an asset
    /// @param assetId Callchain asset identifier
    /// @return remaining Amount that can still be deposited today
    function remainingDailyLimit(uint64 assetId) external view returns (uint128) {
        uint128 used = _getDailyDeposited(assetId);
        return used >= dailyLimit ? 0 : dailyLimit - used;
    }

    /// @notice Get bridge balance for an asset
    /// @param assetId Callchain asset identifier
    function bridgeBalance(uint64 assetId) external view returns (uint128) {
        address token = assetTokens[assetId];
        if (token == address(0)) {
            return uint128(address(this).balance);
        }
        return uint128(IERC20(token).balanceOf(address(this)));
    }

    // ── Internal Functions ─────────────────────────────────────────────

    function _checkLimits(uint64 assetId, uint128 amount) internal {
        if (amount > maxPerTx) revert ExceedsMaxPerTx(amount, maxPerTx);

        // Reset daily counter if a new day has passed
        uint256 currentDay = block.number / BLOCKS_PER_DAY;
        if (currentDay > lastDepositDay[assetId]) {
            dailyDeposited[assetId] = 0;
            lastDepositDay[assetId] = currentDay;
        }

        uint128 newDaily = dailyDeposited[assetId] + amount;
        if (newDaily > dailyLimit) {
            revert ExceedsDailyLimit(assetId, amount, dailyLimit - dailyDeposited[assetId]);
        }
        dailyDeposited[assetId] = newDaily;
    }

    function _getDailyDeposited(uint64 assetId) internal view returns (uint128) {
        uint256 currentDay = block.number / BLOCKS_PER_DAY;
        if (currentDay > lastDepositDay[assetId]) {
            return 0;
        }
        return dailyDeposited[assetId];
    }

    function _safeTransferFrom(address token, address from, address to, uint256 amount) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20.transferFrom.selector, from, to, amount)
        );
        if (!success || (data.length > 0 && !abi.decode(data, (bool)))) {
            revert("TransferFrom failed");
        }
    }

    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20.transfer.selector, to, amount)
        );
        if (!success || (data.length > 0 && !abi.decode(data, (bool)))) {
            revert("Transfer failed");
        }
    }

    // ── Receive ────────────────────────────────────────────────────────

    receive() external payable {
        // Reject direct ETH transfers that are not via depositEth()
        revert("Use depositEth()");
    }
}

/// @dev Minimal IERC20 interface (self-contained, no external dependency)
interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

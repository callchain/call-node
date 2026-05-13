// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./WrappedToken.sol";

/// @title WrappedTokenFactory
/// @notice Deploys system WrappedToken contracts for protocol-dominant assets.
///         Only callable by the Asset precompile (0x201).
contract WrappedTokenFactory {
    /// Asset precompile address (0x201)
    address public constant ASSET_PRECOMPILE = 0x0000000000000000000000000000000000000201;
    /// Switch precompile address (0x207) — acts as bridge
    address public constant BRIDGE = 0x0000000000000000000000000000000000000207;

    /// Mapping from assetId to deployed wrapper address
    mapping(uint256 => address) public wrappers;

    event WrapperCreated(uint256 indexed assetId, address wrapper);

    /// @notice Create a system WrappedToken for a protocol asset.
    /// @param name Token name
    /// @param symbol Token symbol
    /// @param decimals Token decimals
    /// @param assetId Protocol asset identifier
    /// @return wrapper The deployed wrapper contract address
    function createWrapper(
        string calldata name,
        string calldata symbol,
        uint8 decimals,
        uint256 assetId
    ) external returns (address wrapper) {
        require(msg.sender == ASSET_PRECOMPILE, "only asset precompile");
        require(wrappers[assetId] == address(0), "wrapper already exists");

        WrappedToken w = new WrappedToken(
            name,
            symbol,
            decimals,
            BRIDGE,           // bridge = Switch precompile
            address(0),       // issuer = ZERO (no issuerMint)
            0,                // maxSupply = 0 (uncapped, protocol enforces cap)
            assetId
        );

        wrappers[assetId] = address(w);
        emit WrapperCreated(assetId, address(w));
        return address(w);
    }
}

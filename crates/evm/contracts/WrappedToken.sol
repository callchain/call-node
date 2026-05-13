// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

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
    uint256 public assetId;      // Protocol-layer asset identifier

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

    function cap() public view returns (uint256) {
        return _maxSupply;
    }

    function transfer(address _to, uint256 _value) public returns (bool) {
        require(balanceOf[msg.sender] >= _value, "insufficient balance");
        balanceOf[msg.sender] -= _value;
        balanceOf[_to] += _value;
        emit Transfer(msg.sender, _to, _value);
        return true;
    }

    function approve(address _spender, uint256 _value) public returns (bool) {
        allowance[msg.sender][_spender] = _value;
        emit Approval(msg.sender, _spender, _value);
        return true;
    }

    function transferFrom(address _from, address _to, uint256 _value) public returns (bool) {
        require(balanceOf[_from] >= _value, "insufficient balance");
        require(allowance[_from][msg.sender] >= _value, "insufficient allowance");
        balanceOf[_from] -= _value;
        allowance[_from][msg.sender] -= _value;
        balanceOf[_to] += _value;
        emit Transfer(_from, _to, _value);
        return true;
    }

    function bridgeMint(address _to, uint256 _value) public {
        require(msg.sender == bridge, "only bridge");
        totalSupply += _value;
        balanceOf[_to] += _value;
        emit Transfer(address(0), _to, _value);
    }

    function bridgeBurn(address from, uint256 _value) public {
        require(msg.sender == bridge, "only bridge");
        require(balanceOf[from] >= _value, "insufficient balance");
        totalSupply -= _value;
        balanceOf[from] -= _value;
        emit Transfer(from, address(0), _value);
    }

    function issuerMint(address _to, uint256 _value) public {
        require(msg.sender == issuer, "only issuer");
        totalSupply += _value;
        balanceOf[_to] += _value;
        emit Transfer(address(0), _to, _value);
    }
}

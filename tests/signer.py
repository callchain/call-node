"""Transaction signing utilities for Callchain E2E tests.

Uses eth_keys for secp256k1 signing. All write operations now use the unified
`call_submit` endpoint which verifies raw tx_hash signatures.
"""

import json
import struct
from typing import List, Optional

from eth_keys import keys
from eth_hash.auto import keccak


def keccak256(data: bytes) -> bytes:
    return keccak(data)


def eip191_hash(message_hash: bytes) -> bytes:
    """Compute EIP-191 personal_sign hash: keccak256(prefix + message_hash)."""
    prefix = b"\x19Ethereum Signed Message:\n32"
    return keccak256(prefix + message_hash)


def _sign_hash(private_key: str, message_hash: bytes) -> str:
    """Low-level: sign a 32-byte hash with eth_keys. Returns 65-byte hex sig."""
    pk_hex = private_key.removeprefix("0x")
    pk = keys.PrivateKey(bytes.fromhex(pk_hex))
    signature = pk.sign_msg_hash(message_hash)
    # eth_keys returns v as 27/28; convert to 0/1 for k256 compatibility
    sig_bytes = bytes.fromhex(signature.to_hex()[2:])  # strip 0x prefix
    v = sig_bytes[64]
    if v >= 27:
        v -= 27
    sig_bytes = sig_bytes[:64] + bytes([v])
    return "0x" + sig_bytes.hex()


def sign_eip191(private_key: str, message_hash: bytes) -> str:
    """Sign an EIP-191 wrapped message. Returns 65-byte hex signature."""
    eth_hash = eip191_hash(message_hash)
    return _sign_hash(private_key, eth_hash)


def sign_raw(private_key: str, message_hash: bytes) -> str:
    """Sign a raw 32-byte hash directly. Returns 65-byte hex signature."""
    return _sign_hash(private_key, message_hash)


def compute_tx_hash(
    sender: str,
    nonce: int,
    instructions: List[dict],
    gas_limit: int = 100_000,
    max_fee: int = 1_000_000,
    max_priority_fee: int = 1,
    expires_at: int = 0,
) -> bytes:
    """Compute the canonical ProtocolTransaction hash.

    This must exactly match ProtocolTransaction::compute_tx_hash() in Rust:
      1. sender (20 bytes)
      2. nonce (8 bytes, big-endian u64)
      3. instructions (JSON bytes)
      4. gas_config tag (1 byte: 0=SelfPay)
      5. fee_currency tag (1 byte: 0=Call)
      6. gas_limit (8 bytes, big-endian u64)
      7. max_fee (16 bytes, big-endian u128)
      8. max_priority_fee (16 bytes, big-endian u128)
      9. expires_at (8 bytes, big-endian u64)
     10. keccak256(preimage)
    """
    preimage = bytearray()

    # 1. sender address (20 bytes)
    preimage.extend(bytes.fromhex(sender.removeprefix("0x")))

    # 2. nonce (u64 big-endian)
    preimage.extend(struct.pack(">Q", nonce))

    # 3. instructions JSON
    instr_json = json.dumps(instructions, separators=(",", ":"))
    preimage.extend(instr_json.encode("utf-8"))

    # 4. gas_config tag (SelfPay = 0)
    preimage.append(0)

    # 5. fee_currency tag (Call = 0)
    preimage.append(0)

    # 6. gas_limit (u64 big-endian)
    preimage.extend(struct.pack(">Q", gas_limit))

    # 7. max_fee (u128 big-endian)
    preimage.extend(struct.pack(">QQ", max_fee >> 64, max_fee & 0xFFFFFFFFFFFFFFFF))

    # 8. max_priority_fee (u128 big-endian)
    preimage.extend(struct.pack(">QQ", max_priority_fee >> 64, max_priority_fee & 0xFFFFFFFFFFFFFFFF))

    # 9. expires_at (u64 big-endian)
    preimage.extend(struct.pack(">Q", expires_at))

    # 10. keccak256
    return keccak256(bytes(preimage))


def _norm_addr(addr: str) -> str:
    """Normalize address to lowercase hex (matches Rust alloy-primitives serde)."""
    return addr.lower()


def build_transfer_instruction(asset_id: int, to: str, amount: int, memo: Optional[str] = None) -> dict:
    """Build a Transfer instruction dict matching Rust serde format."""
    instr = {
        "Transfer": {
            "asset_id": asset_id,
            "to": _norm_addr(to),
            "amount": amount,
            "memo": None,
        }
    }
    if memo is not None:
        instr["Transfer"]["memo"] = {
            "message": memo,
            "reference": None,
            "metadata": None,
        }
    return instr


def sign_payment(
    private_key: str,
    sender: str,
    nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    memo: Optional[str] = None,
    gas_limit: int = 100_000,
    max_fee: int = 1_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed payment payload for call_submit."""
    instructions = [build_transfer_instruction(asset_id, to, amount, memo)]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_asset_registration(
    private_key: str,
    sender: str,
    nonce: int,
    symbol: str,
    name: str,
    decimals: int,
    max_supply: int = 0,
    gas_limit: int = 200_000,
    max_fee: int = 2_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed asset registration payload for call_submit."""
    instructions = [{
        "RegisterAsset": {
            "symbol": symbol,
            "name": name,
            "decimals": decimals,
            "max_supply": max_supply,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_agent_register(
    private_key: str,
    sender: str,
    nonce: int,
    pubkey_hex: str,
    name: str,
    url: str,
    gas_limit: int = 100_000,
    max_fee: int = 1_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed agent registration payload for call_submit."""
    pubkey_bytes = bytes.fromhex(pubkey_hex.removeprefix("0x"))
    instructions = [{
        "RegisterAgent": {
            "pubkey": list(pubkey_bytes),
            "name": name,
            "url": url,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_governance_proposal(
    private_key: str,
    sender: str,
    nonce: int,
    proposal_type: str,
    title: str,
    description: str,
    execution_data: bytes = b"",
    gas_limit: int = 200_000,
    max_fee: int = 2_000_000,
    max_priority_fee: int = 1,
    **type_params,
) -> dict:
    """Build a signed governance proposal payload."""
    # Governance uses raw tx_hash signing (not EIP-191)
    if proposal_type == "ParameterChange":
        pt = {"ParameterChange": {"param_id": type_params["param_id"], "new_value": type_params["new_value"]}}
    elif proposal_type == "ProtocolUpgrade":
        pt = {"ProtocolUpgrade": {"activation_block": type_params["activation_block"], "changelog": type_params["changelog"]}}
    elif proposal_type == "TreasurySpend":
        pt = {"TreasurySpend": {"recipient": _norm_addr(type_params["recipient"]), "amount": str(type_params["amount"]), "asset_id": type_params["asset_id"]}}
    elif proposal_type == "EmergencyPause":
        pt = {"EmergencyPause": {"reason": type_params["reason"]}}
    else:
        raise ValueError(f"unknown proposal_type: {proposal_type}")

    instructions = [{
        "GovernanceSubmitProposal": {
            "proposal_type": pt,
            "title": title,
            "description": description,
            "execution_data": list(execution_data),
        }
    }]

    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "proposalType": pt,
        "type": proposal_type,
        "title": title,
        "description": description,
        "executionData": execution_data.hex() if execution_data else "",
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_governance_vote(
    private_key: str,
    voter: str,
    nonce: int,
    proposal_id: int,
    vote: str,
    gas_limit: int = 50_000,
    max_fee: int = 500_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed governance vote payload."""
    # Rust Vote enum serializes as "Yes" / "No" / "Abstain"
    rust_vote = vote.capitalize()
    instructions = [{
        "GovernanceVote": {
            "proposal_id": proposal_id,
            "vote": rust_vote,
        }
    }]
    tx_hash = compute_tx_hash(voter, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": voter,
        "nonce": nonce,
        "proposalId": proposal_id,
        "vote": vote,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_validator_stake(
    private_key: str,
    sender: str,
    nonce: int,
    ed25519_pubkey_hex: str,
    self_stake: int,
    gas_limit: int = 200_000,
    max_fee: int = 2_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed validator stake payload for call_validatorStake RPC."""
    pubkey_bytes = bytes.fromhex(ed25519_pubkey_hex.removeprefix("0x"))
    instructions = [{
        "ValidatorStake": {
            "ed25519_pubkey": list(pubkey_bytes),
            "self_stake": self_stake,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "ed25519Pubkey": ed25519_pubkey_hex,
        "selfStake": str(self_stake),
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_validator_unstake(
    private_key: str,
    sender: str,
    nonce: int,
    validator_id: int,
    gas_limit: int = 100_000,
    max_fee: int = 1_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed validator unstake payload for call_validatorUnstake RPC."""
    instructions = [{
        "ValidatorUnstake": {
            "validator_id": validator_id,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "validatorId": validator_id,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_bridge_to_evm(
    private_key: str,
    sender: str,
    nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas_limit: int = 25_000,
    max_fee: int = 250_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed BridgeToEvm payload for call_bridgeToEvm RPC."""
    instructions = [{
        "BridgeToEvm": {
            "asset_id": asset_id,
            "to": _norm_addr(to),
            "amount": amount,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "assetId": asset_id,
        "to": to,
        "amount": str(amount),
        "signature": signature,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_bridge_to_protocol(
    private_key: str,
    sender: str,
    nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas_limit: int = 25_000,
    max_fee: int = 250_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed BridgeToProtocol payload for call_bridgeToProtocol RPC."""
    instructions = [{
        "BridgeToProtocol": {
            "asset_id": asset_id,
            "to": _norm_addr(to),
            "amount": amount,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "assetId": asset_id,
        "to": to,
        "amount": str(amount),
        "signature": signature,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


def sign_validator_claim_unbonded(
    private_key: str,
    sender: str,
    nonce: int,
    validator_id: int,
    gas_limit: int = 100_000,
    max_fee: int = 1_000_000,
    max_priority_fee: int = 1,
) -> dict:
    """Build a signed validator claim unbonded payload for call_validatorClaimUnbonded RPC."""
    instructions = [{
        "ValidatorClaimUnbonded": {
            "validator_id": validator_id,
        }
    }]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee, max_priority_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "sender": sender,
        "nonce": nonce,
        "validatorId": validator_id,
        "signature": signature,
        "instructions": instructions,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
        "maxPriorityFee": max_priority_fee,
    }


# ── EVM Precompile Transaction Helpers ────────────────────────────────

def build_evm_transfer_data(asset_id: int, to: str, amount: int) -> str:
    """Build ABI-encoded call data for transfer(uint64,address,uint128) on 0x201."""
    from eth_abi import encode
    selector = bytes.fromhex("d15dcd62")
    encoded = encode(
        ["uint64", "address", "uint128"],
        [asset_id, to, amount],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_batch_transfer_data(asset_id: int, recipients: List[str], amounts: List[int]) -> str:
    """Build ABI-encoded call data for batchTransfer(uint64,address[],uint128[]) on 0x201."""
    from eth_abi import encode
    selector = bytes.fromhex("5f9161bb")
    encoded = encode(
        ["uint64", "address[]", "uint128[]"],
        [asset_id, recipients, amounts],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_stake_data(ed25519_pubkey_hex: str, self_stake: int) -> str:
    """Build ABI-encoded call data for stake(bytes32,uint256) on 0x204."""
    from eth_abi import encode
    selector = bytes.fromhex("8caa5230")
    pubkey_bytes = bytes.fromhex(ed25519_pubkey_hex.removeprefix("0x"))
    if len(pubkey_bytes) != 32:
        raise ValueError(f"ed25519 pubkey must be 32 bytes, got {len(pubkey_bytes)}")
    encoded = encode(
        ["bytes32", "uint256"],
        [pubkey_bytes, self_stake],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_unstake_data(validator_id: int) -> str:
    """Build ABI-encoded call data for unstake(uint32) on 0x204."""
    from eth_abi import encode
    selector = bytes.fromhex("809ee57d")
    encoded = encode(
        ["uint32"],
        [validator_id],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_claim_unbonded_data(validator_id: int) -> str:
    """Build ABI-encoded call data for claimUnbonded(uint32) on 0x204."""
    from eth_abi import encode
    selector = bytes.fromhex("e8f59072")
    encoded = encode(
        ["uint32"],
        [validator_id],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_register_asset_data(symbol: str, name: str, decimals: int, max_supply: int) -> str:
    """Build ABI-encoded call data for register(string,string,uint8,uint256) on 0x201."""
    from eth_abi import encode
    selector = bytes.fromhex("484a573d")
    encoded = encode(
        ["string", "string", "uint8", "uint256"],
        [symbol, name, decimals, max_supply],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_register_agent_data(pubkey_hex: str, name: str, url: str) -> str:
    """Build ABI-encoded call data for registerAgent(bytes,string,string) on 0x209."""
    from eth_abi import encode
    selector = bytes.fromhex("c0fc6063")
    pubkey_bytes = bytes.fromhex(pubkey_hex.removeprefix("0x"))
    encoded = encode(
        ["bytes", "string", "string"],
        [pubkey_bytes, name, url],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_submit_proposal_data(proposal_type: int, title: str, description: str, execution_data: bytes) -> str:
    """Build ABI-encoded call data for submitProposal(uint8,string,string,bytes) on 0x203."""
    from eth_abi import encode
    selector = bytes.fromhex("5830d3ea")
    encoded = encode(
        ["uint8", "string", "string", "bytes"],
        [proposal_type, title, description, execution_data],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_vote_data(proposal_id: int, vote: int) -> str:
    """Build ABI-encoded call data for vote(uint64,uint8) on 0x203."""
    from eth_abi import encode
    selector = bytes.fromhex("b040d166")
    encoded = encode(
        ["uint64", "uint8"],
        [proposal_id, vote],
    )
    return "0x" + (selector + encoded).hex()


def sign_evm_transaction(
    private_key: str,
    nonce: int,
    to: str,
    data: str,
    gas: int = 200_000,
    gas_price: int = 1,
    value: int = 0,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction and return the raw RLP-encoded hex string.

    Uses eth_account for full RLP encoding and secp256k1 signing.
    """
    from eth_account import Account

    tx = {
        "nonce": nonce,
        "gasPrice": gas_price,
        "gas": gas,
        "to": to,
        "value": value,
        "data": data,
        "chainId": chain_id,
    }
    signed = Account.sign_transaction(tx, private_key)
    return "0x" + signed.raw_transaction.hex()


# ── Convenience wrappers for common precompile operations ─────────────

def sign_evm_precompile_transfer(
    private_key: str,
    evm_nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas: int = 100_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling transfer() on Asset precompile (0x201)."""
    data = build_evm_transfer_data(asset_id, to, amount)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000201",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_stake(
    private_key: str,
    evm_nonce: int,
    ed25519_pubkey_hex: str,
    self_stake: int,
    gas: int = 200_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling stake() on Validator precompile (0x204)."""
    data = build_evm_stake_data(ed25519_pubkey_hex, self_stake)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000204",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_unstake(
    private_key: str,
    evm_nonce: int,
    validator_id: int,
    gas: int = 150_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling unstake() on Validator precompile (0x204)."""
    data = build_evm_unstake_data(validator_id)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000204",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_claim_unbonded(
    private_key: str,
    evm_nonce: int,
    validator_id: int,
    gas: int = 150_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling claimUnbonded() on Validator precompile (0x204)."""
    data = build_evm_claim_unbonded_data(validator_id)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000204",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_register_asset(
    private_key: str,
    evm_nonce: int,
    symbol: str,
    name: str,
    decimals: int,
    max_supply: int,
    gas: int = 200_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling register() on Asset precompile (0x201)."""
    data = build_evm_register_asset_data(symbol, name, decimals, max_supply)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000201",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_register_agent(
    private_key: str,
    evm_nonce: int,
    pubkey_hex: str,
    name: str,
    url: str,
    gas: int = 200_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling registerAgent() on Agent precompile (0x209)."""
    data = build_evm_register_agent_data(pubkey_hex, name, url)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000209",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_submit_proposal(
    private_key: str,
    evm_nonce: int,
    proposal_type: int,
    title: str,
    description: str,
    execution_data: bytes = b"",
    gas: int = 200_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling submitProposal() on Governance precompile (0x203)."""
    data = build_evm_submit_proposal_data(proposal_type, title, description, execution_data)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000203",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_vote(
    private_key: str,
    evm_nonce: int,
    proposal_id: int,
    vote: int,
    gas: int = 100_000,
    gas_price: int = 1,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling vote() on Governance precompile (0x203)."""
    data = build_evm_vote_data(proposal_id, vote)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to="0x0000000000000000000000000000000000000203",
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )

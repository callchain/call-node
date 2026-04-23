"""Transaction signing utilities for Callchain E2E tests.

Uses eth_keys for secp256k1 signing. Supports EIP-191 personal_sign
(required by call_sendPayment RPC) and raw tx_hash signing.

KNOWN ISSUE: The current codebase has a signature verification mismatch:
- RPC handler call_sendPayment verifies EIP-191 signatures
- Block execution (Block::execute) verifies raw tx_hash signatures
This means transactions submitted via RPC currently fail during block execution.
For E2E testing, we use EIP-191 to match the RPC handler.
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
      8. expires_at (8 bytes, big-endian u64)
      9. keccak256(preimage)
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

    # 8. expires_at (u64 big-endian)
    preimage.extend(struct.pack(">Q", expires_at))

    # 9. keccak256
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
) -> dict:
    """Build a signed payment payload for call_sendPayment RPC."""
    instructions = [build_transfer_instruction(asset_id, to, amount, memo)]
    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee)
    signature = sign_eip191(private_key, tx_hash)

    return {
        "from": sender,
        "to": to,
        "amount": str(amount),
        "nonce": nonce,
        "assetId": asset_id,
        "signature": signature,
        "gasLimit": gas_limit,
        "maxFee": max_fee,
    }


def sign_asset_registration(
    private_key: str,
    symbol: str,
    name: str,
    decimals: int,
    issuer: str,
) -> str:
    """Sign an asset registration message for call_registerAsset RPC."""
    issuer_hex = issuer.removeprefix("0x").lower()
    canonical = f"RegisterAsset:{symbol}:{name}:{decimals}:{issuer_hex}"
    raw_hash = keccak256(canonical.encode("utf-8"))
    return sign_eip191(private_key, raw_hash)


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

    tx_hash = compute_tx_hash(sender, nonce, instructions, gas_limit, max_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "proposer": sender,
        "nonce": nonce,
        "proposalType": pt,
        "type": proposal_type,
        "title": title,
        "description": description,
        "executionData": execution_data.hex() if execution_data else "",
        "signature": signature,
    }


def sign_governance_vote(
    private_key: str,
    voter: str,
    nonce: int,
    proposal_id: int,
    vote: str,
    gas_limit: int = 50_000,
    max_fee: int = 500_000,
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
    tx_hash = compute_tx_hash(voter, nonce, instructions, gas_limit, max_fee)
    signature = sign_raw(private_key, tx_hash)

    return {
        "voter": voter,
        "nonce": nonce,
        "proposalId": proposal_id,
        "vote": vote,
        "signature": signature,
    }

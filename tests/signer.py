"""EVM transaction signing utilities for Callchain E2E tests.

All protocol operations are accessed via EVM precompiles (0x101-0x209).
Uses eth_keys/eth_account for secp256k1 signing.
"""

from eth_abi import encode
from eth_account import Account
from typing import List


def build_evm_transfer_data(asset_id: int, to: str, amount: int) -> str:
    """Build ABI-encoded call data for transfer(uint64,address,uint128) on 0x201."""
    selector = bytes.fromhex("996e62b5")
    encoded = encode(
        ["uint64", "address", "uint128"],
        [asset_id, to, amount],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_batch_transfer_data(asset_id: int, recipients: List[str], amounts: List[int]) -> str:
    """Build ABI-encoded call data for batchTransfer(uint64,address[],uint128[]) on 0x201."""
    selector = bytes.fromhex("d5b6e817")
    encoded = encode(
        ["uint64", "address[]", "uint128[]"],
        [asset_id, recipients, amounts],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_stake_data(ed25519_pubkey_hex: str, self_stake: int) -> str:
    """Build ABI-encoded call data for stake(bytes32,uint128) on 0x204."""
    selector = bytes.fromhex("48720640")
    pubkey_bytes = bytes.fromhex(ed25519_pubkey_hex.removeprefix("0x"))
    if len(pubkey_bytes) != 32:
        raise ValueError(f"ed25519 pubkey must be 32 bytes, got {len(pubkey_bytes)}")
    encoded = encode(
        ["bytes32", "uint128"],
        [pubkey_bytes, self_stake],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_unstake_data(validator_id: int) -> str:
    """Build ABI-encoded call data for unstake(uint64) on 0x204."""
    selector = bytes.fromhex("d29ab87a")
    encoded = encode(
        ["uint64"],
        [validator_id],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_claim_unbonded_data(validator_id: int) -> str:
    """Build ABI-encoded call data for claimUnbonded(uint64) on 0x204."""
    selector = bytes.fromhex("6ab76049")
    encoded = encode(
        ["uint64"],
        [validator_id],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_register_asset_data(symbol: str, name: str, decimals: int, max_supply: int) -> str:
    """Build ABI-encoded call data for register(string,string,uint8,uint128) on 0x201."""
    selector = bytes.fromhex("65716710")
    encoded = encode(
        ["string", "string", "uint8", "uint128"],
        [symbol, name, decimals, max_supply],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_register_agent_data(pubkey_hex: str, name: str, url: str) -> str:
    """Build ABI-encoded call data for register(bytes,string,string) on 0x209."""
    selector = bytes.fromhex("4d431f19")
    pubkey_bytes = bytes.fromhex(pubkey_hex.removeprefix("0x"))
    encoded = encode(
        ["bytes", "string", "string"],
        [pubkey_bytes, name, url],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_submit_proposal_data(proposal_type: int, title: str, description: str, execution_data: bytes) -> str:
    """Build ABI-encoded call data for submitProposal(uint8,string,string,bytes) on 0x203."""
    selector = bytes.fromhex("5830d3ea")
    encoded = encode(
        ["uint8", "string", "string", "bytes"],
        [proposal_type, title, description, execution_data],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_vote_data(proposal_id: int, vote: int) -> str:
    """Build ABI-encoded call data for vote(uint64,uint8) on 0x203."""
    selector = bytes.fromhex("b040d166")
    encoded = encode(
        ["uint64", "uint8"],
        [proposal_id, vote],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_switch_to_evm_data(asset_id: int, to: str, amount: int) -> str:
    """Build ABI-encoded call data for switchToEvm(uint64,address,uint128) on 0x207."""
    selector = bytes.fromhex("4a4883bc")
    encoded = encode(
        ["uint64", "address", "uint128"],
        [asset_id, to, amount],
    )
    return "0x" + (selector + encoded).hex()


def build_evm_switch_to_protocol_data(asset_id: int, to: str, amount: int) -> str:
    """Build ABI-encoded call data for switchToProtocol(uint64,address,uint128) on 0x207."""
    selector = bytes.fromhex("7aa6732c")
    encoded = encode(
        ["uint64", "address", "uint128"],
        [asset_id, to, amount],
    )
    return "0x" + (selector + encoded).hex()


def sign_evm_transaction(
    private_key: str,
    nonce: int,
    to: str,
    data: str,
    gas: int = 200_000,
    gas_price: int = 1000,
    value: int = 0,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction and return the raw RLP-encoded hex string."""
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

ASSET_ADDRESS = "0x0000000000000000000000000000000000000201"
VALIDATOR_ADDRESS = "0x0000000000000000000000000000000000000204"
GOVERNANCE_ADDRESS = "0x0000000000000000000000000000000000000203"
AGENT_ADDRESS = "0x0000000000000000000000000000000000000209"
SWITCH_ADDRESS = "0x0000000000000000000000000000000000000207"


def sign_evm_precompile_transfer(
    private_key: str,
    evm_nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas: int = 100_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling transfer() on Asset precompile (0x201)."""
    data = build_evm_transfer_data(asset_id, to, amount)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=ASSET_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_stake(
    private_key: str,
    evm_nonce: int,
    ed25519_pubkey_hex: str,
    self_stake: int,
    gas: int = 200_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling stake() on Validator precompile (0x204)."""
    data = build_evm_stake_data(ed25519_pubkey_hex, self_stake)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=VALIDATOR_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_unstake(
    private_key: str,
    evm_nonce: int,
    validator_id: int,
    gas: int = 150_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling unstake() on Validator precompile (0x204)."""
    data = build_evm_unstake_data(validator_id)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=VALIDATOR_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_claim_unbonded(
    private_key: str,
    evm_nonce: int,
    validator_id: int,
    gas: int = 150_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling claimUnbonded() on Validator precompile (0x204)."""
    data = build_evm_claim_unbonded_data(validator_id)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=VALIDATOR_ADDRESS,
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
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling register() on Asset precompile (0x201)."""
    data = build_evm_register_asset_data(symbol, name, decimals, max_supply)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=ASSET_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_register_agent(
    private_key: str,
    evm_nonce: int,
    pubkey_hex: str,
    name: str,
    url: str,
    gas: int = 200_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling register() on Agent precompile (0x209)."""
    data = build_evm_register_agent_data(pubkey_hex, name, url)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=AGENT_ADDRESS,
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
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling submitProposal() on Governance precompile (0x203)."""
    data = build_evm_submit_proposal_data(proposal_type, title, description, execution_data)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=GOVERNANCE_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_vote(
    private_key: str,
    evm_nonce: int,
    proposal_id: int,
    vote: int,
    gas: int = 100_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling vote() on Governance precompile (0x203)."""
    data = build_evm_vote_data(proposal_id, vote)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=GOVERNANCE_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_switch_to_evm(
    private_key: str,
    evm_nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas: int = 100_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling switchToEvm() on Switch precompile (0x207)."""
    data = build_evm_switch_to_evm_data(asset_id, to, amount)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=SWITCH_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )


def sign_evm_precompile_switch_to_protocol(
    private_key: str,
    evm_nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas: int = 100_000,
    gas_price: int = 1000,
    chain_id: int = 1,
) -> str:
    """Sign an EVM transaction calling switchToProtocol() on Switch precompile (0x207)."""
    data = build_evm_switch_to_protocol_data(asset_id, to, amount)
    return sign_evm_transaction(
        private_key, evm_nonce,
        to=SWITCH_ADDRESS,
        data=data, gas=gas, gas_price=gas_price, chain_id=chain_id,
    )

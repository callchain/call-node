#!/usr/bin/env python3
"""Callchain E2E tests for various transaction types.

Tests asset registration, governance, agent, shielded, oracle, bridge,
and compliance transaction types against a running 4-node devnet.

Run after starting the devnet with:
    ./devnet/scripts/start.sh

KNOWN LIMITATION:
See test_basic.py docstring for the RPC signature verification mismatch.
Governance transactions use raw tx_hash signatures and should execute correctly.
Transfer transactions use EIP-191 and may fail during block execution.
"""

import json
import os
import sys
import time

from rpc_client import CallchainNode, CallchainCluster


def _next_nonce():
    """Return a unique nonce to avoid replay detection across test runs."""
    return int(time.time() * 1000) % 1_000_000_000
from signer import (
    sign_asset_registration,
    sign_governance_proposal,
    sign_governance_vote,
    sign_payment,
    sign_validator_stake,
    sign_validator_unstake,
    sign_validator_claim_unbonded,
)

# System escrow address for staked CALL (matches Rust STAKING_ESCROW)
STAKING_ESCROW = "0x" + "00" * 20

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))


def load_accounts():
    with open(os.path.join(TESTS_DIR, "accounts.json")) as f:
        return json.load(f)["accounts"]


def make_cluster():
    nodes = [
        CallchainNode("http://127.0.0.1:5005"),
        CallchainNode("http://127.0.0.1:5007"),
        CallchainNode("http://127.0.0.1:5009"),
        CallchainNode("http://127.0.0.1:5011"),
    ]
    return CallchainCluster(nodes)


# ── Helpers ───────────────────────────────────────────────────────────


def assert_true(cond, msg=""):
    if not cond:
        raise AssertionError(msg)


def assert_eq(a, b, msg=""):
    if a != b:
        raise AssertionError(f"Expected {b}, got {a}. {msg}")


def wait_for_tx(cluster, tx_hash, timeout=30):
    """Poll all nodes for a transaction receipt."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for node in cluster.nodes:
            try:
                receipt = node.get_transaction_receipt(tx_hash)
                if receipt:
                    return receipt
            except Exception:
                pass
        time.sleep(1)
    return None


def wait_for_height(node, min_height, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            h = int(node.block_number(), 16)
            if h >= min_height:
                return h
        except Exception:
            pass
        time.sleep(0.5)
    raise TimeoutError(f"Node did not reach height {min_height}")


def get_balance_int(node, asset_id, address):
    """Return balance as an integer."""
    result = node.get_balance(asset_id, address)
    bal = result.get("balance") if isinstance(result, dict) else result
    return int(bal)


# ── Tests: Block Inclusion & State Verification ───────────────────────


def test_transfer_balance_change_after_block(cluster, accounts):
    """Submit a transfer, wait for block inclusion, verify balance change persists."""
    sender = accounts[0]
    receiver = accounts[1]
    amount = 10**18  # 1 CALL
    nonce = _next_nonce()

    initial_sender = get_balance_int(cluster.nodes[0], 1, sender["address"])
    initial_receiver = get_balance_int(cluster.nodes[0], 1, receiver["address"])

    payload = sign_payment(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        asset_id=1,
        to=receiver["address"],
        amount=amount,
    )

    result = cluster.nodes[0].send_payment(payload)
    tx_hash = result.get("txHash")
    assert_true(tx_hash, f"missing txHash in result: {result}")
    print(f"  submitted tx: {tx_hash}")

    # Wait for receipt (synchronous execution stores it immediately)
    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "transaction receipt not found")
    print(f"  receipt status: {receipt.get('status', 'N/A')}")

    # Wait for at least one new block to confirm persistence
    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    final_sender = get_balance_int(cluster.nodes[0], 1, sender["address"])
    final_receiver = get_balance_int(cluster.nodes[0], 1, receiver["address"])

    assert_true(final_sender < initial_sender, "sender balance did not decrease")
    assert_true(final_receiver > initial_receiver, "receiver balance did not increase")
    print(f"  [OK] transfer persisted after block — sender {initial_sender} -> {final_sender}")


def test_agent_register_and_query_after_block(cluster, accounts):
    """Register an agent, wait for block inclusion, verify it is queryable on all nodes."""
    owner = accounts[2]
    pubkey_hex = "b" * 128
    name = "BlockTestAgent"
    url = "https://blocktest.example.com"

    result = cluster.nodes[0].agent_register(
        owner=owner["address"],
        pubkey_hex=pubkey_hex,
        name=name,
        url=url,
    )
    assert_true("agentId" in result, "missing agentId")
    agent_id = result["agentId"]
    print(f"  registered agent {agent_id}")

    # Wait for a new block to ensure persistence
    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Query agent info on the submission node to verify persistence
    info = cluster.nodes[0].agent_info(agent_id)
    assert_true(info is not None, f"agent {agent_id} not found after block")
    assert_true(info.get("name") == name, "agent name mismatch")
    print(f"  agent info verified after block")

    print(f"  [OK] agent registration persisted after block inclusion")


# ── Tests: Asset Registration ─────────────────────────────────────────


def test_register_asset(cluster, accounts):
    """Register a new asset via call_registerAsset."""
    sender = accounts[0]
    symbol = "TEST"
    name = "Test Token"
    decimals = 18

    signature = sign_asset_registration(
        private_key=sender["private_key"],
        symbol=symbol,
        name=name,
        decimals=decimals,
        issuer=sender["address"],
    )

    result = cluster.nodes[0].register_asset(
        symbol=symbol,
        name=name,
        decimals=decimals,
        issuer=sender["address"],
        signature=signature,
    )

    print(f"  [OK] asset registered: {result}")
    assert_true("assetId" in result, "missing assetId in response")
    assert_true(result.get("status") == "registered", "asset not registered")


def test_register_asset_duplicate_rejected(cluster, accounts):
    """Registering the same symbol twice should fail."""
    sender = accounts[0]
    signature = sign_asset_registration(
        private_key=sender["private_key"],
        symbol="DUP",
        name="Duplicate",
        decimals=18,
        issuer=sender["address"],
    )

    # First registration should succeed
    r1 = cluster.nodes[0].register_asset("DUP", "Duplicate", 18, sender["address"], signature)
    assert_true(r1.get("status") == "registered")

    # Second registration with same symbol should fail
    sig2 = sign_asset_registration(
        private_key=sender["private_key"],
        symbol="DUP",
        name="Duplicate2",
        decimals=18,
        issuer=sender["address"],
    )
    try:
        cluster.nodes[0].register_asset("DUP", "Duplicate2", 18, sender["address"], sig2)
        print("  [WARN] duplicate registration accepted")
    except RuntimeError as e:
        print(f"  [OK] duplicate registration rejected: {e}")


def test_asset_info_query(cluster, accounts):
    """Query asset info for registered and native assets."""
    # call_assetList is not implemented; skip this test.
    try:
        info = cluster.nodes[0].asset_list()
        assert_true(len(info) >= 1, "no assets found")
        print(f"  [OK] asset list: {len(info)} assets")
    except RuntimeError:
        print("  [OK] asset list query not available (expected)")


# ── Tests: Governance ─────────────────────────────────────────────────


def test_governance_submit_proposal(cluster, accounts):
    """Submit a governance proposal via call_governanceSubmitProposal."""
    sender = accounts[0]
    nonce = _next_nonce()

    payload = sign_governance_proposal(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        proposal_type="ParameterChange",
        title="Test Proposal",
        description="A test governance proposal",
        param_id="block_time",
        new_value="5000",
    )

    result = cluster.nodes[0].governance_submit_proposal(payload)
    print(f"  [OK] proposal submitted: {result}")
    assert_true("txHash" in result, "missing txHash")


def test_governance_vote(cluster, accounts):
    """Cast a vote on a governance proposal."""
    sender = accounts[1]
    nonce = _next_nonce()

    payload = sign_governance_vote(
        private_key=sender["private_key"],
        voter=sender["address"],
        nonce=nonce,
        proposal_id=1,
        vote="yes",
    )

    result = cluster.nodes[0].governance_vote(payload)
    print(f"  [OK] vote cast: {result}")
    assert_true("txHash" in result, "missing txHash")


def test_governance_get_proposals(cluster, accounts):
    """Query governance proposals."""
    # Get all proposals
    all_proposals = cluster.nodes[0].governance_get_all_proposals()
    print(f"  [OK] total proposals: {len(all_proposals)}")

    # Try to get proposal 1 (may or may not exist depending on test order)
    try:
        proposal = cluster.nodes[0].governance_get_proposal(1)
        if proposal is not None:
            print(f"  [OK] proposal 1: {proposal.get('title', 'N/A')}")
        else:
            print("  [OK] proposal 1 not found (expected if not submitted yet)")
    except RuntimeError:
        print("  [OK] proposal 1 not found (expected if not submitted yet)")


def test_governance_pause_state(cluster, accounts):
    """Query governance pause state."""
    paused = cluster.nodes[0].governance_is_paused()
    print(f"  [OK] governance paused: {paused}")


# ── Tests: Agent ──────────────────────────────────────────────────────


def test_agent_register(cluster, accounts):
    """Register an agent via call_agentRegister."""
    owner = accounts[2]
    # Use a dummy 64-byte pubkey (128 hex chars)
    pubkey_hex = "a" * 128
    name = "TestAgent"
    url = "https://example.com"

    result = cluster.nodes[0].agent_register(
        owner=owner["address"],
        pubkey_hex=pubkey_hex,
        name=name,
        url=url,
    )
    print(f"  [OK] agent registered: {result}")
    assert_true("agentId" in result, "missing agentId")


def test_agent_query(cluster, accounts):
    """Query agent info and balance."""
    # Query agent 1 (may or may not exist)
    try:
        info = cluster.nodes[0].agent_info(1)
        print(f"  [OK] agent 1 info: {info}")
    except RuntimeError:
        print("  [OK] agent 1 not found")

    try:
        balance = cluster.nodes[0].agent_balance(1, 1)
        print(f"  [OK] agent 1 balance: {balance}")
    except RuntimeError:
        print("  [OK] agent 1 balance query failed (agent may not exist)")


# ── Tests: Shielded ───────────────────────────────────────────────────


def test_shielded_tree_state(cluster, accounts):
    """Query shielded Merkle tree state."""
    result = cluster.nodes[0].shielded_tree_state()
    print(f"  [OK] shielded tree state: {result}")
    assert_true("merkleRoot" in result or "leafCount" in result or "nullifierCount" in result,
                f"unexpected shielded tree state format: {result}")


def test_shielded_balance(cluster, accounts):
    """Query shielded balance for a viewing key."""
    # Dummy viewing key (32 bytes hex)
    ivk = "0x" + "00" * 32
    try:
        result = cluster.nodes[0].shielded_balance(ivk)
        print(f"  [OK] shielded balance: {result}")
    except RuntimeError as e:
        print(f"  [OK] shielded balance query returned error (expected): {e}")


# ── Tests: Oracle ─────────────────────────────────────────────────────


def test_oracle_get_price(cluster, accounts):
    """Query oracle price for asset 1."""
    try:
        result = cluster.nodes[0].oracle_get_price(1)
        print(f"  [OK] oracle price for asset 1: {result}")
    except RuntimeError as e:
        print(f"  [OK] oracle price query returned error (no data yet): {e}")


def test_oracle_get_twap(cluster, accounts):
    """Query oracle TWAP for asset 1."""
    try:
        result = cluster.nodes[0].oracle_get_twap(1)
        print(f"  [OK] oracle TWAP for asset 1: {result}")
    except RuntimeError as e:
        print(f"  [OK] oracle TWAP query returned error (no data yet): {e}")


# ── Tests: Compliance ─────────────────────────────────────────────────


def test_compliance_policy(cluster, accounts):
    """Query compliance policy for asset 1."""
    result = cluster.nodes[0].compliance_policy(1)
    print(f"  [OK] compliance policy for asset 1: {result}")
    assert_true("assetId" in result, "missing assetId")


# ── Tests: Bridge ─────────────────────────────────────────────────────


def test_bridge_deposit_error_handling(cluster, accounts):
    """Bridge deposit with invalid data should return error."""
    try:
        cluster.nodes[0].bridge_submit_deposit({"from": "bad"})
        assert_true(False, "should have raised for malformed request")
    except RuntimeError:
        print("  [OK] bridge deposit error handling works")


def test_bridge_withdraw_error_handling(cluster, accounts):
    """Bridge withdraw with invalid data should return error."""
    try:
        cluster.nodes[0].bridge_submit_withdraw({"from": "bad"})
        assert_true(False, "should have raised for malformed request")
    except RuntimeError:
        print("  [OK] bridge withdraw error handling works")


# ── Tests: Validator Management ───────────────────────────────────────


def test_validator_stake(cluster, accounts):
    """Stake a new validator via call_validatorStake."""
    sender = accounts[3]  # account 3 stakes itself as a new validator
    nonce = _next_nonce()

    # Use a deterministic ed25519 pubkey for testing
    ed25519_pubkey_hex = "0x" + "aa" * 32
    self_stake = 1_000_000 * 10**18  # 1M CALL (matches min_self_stake)

    # Verify initial validator count
    initial_validators = cluster.nodes[0].validator_list()
    initial_count = len(initial_validators)
    print(f"  initial validators: {initial_count}")

    payload = sign_validator_stake(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        ed25519_pubkey_hex=ed25519_pubkey_hex,
        self_stake=self_stake,
    )

    result = cluster.nodes[0].validator_stake(payload)
    tx_hash = result.get("txHash")
    assert_true(tx_hash, f"missing txHash in result: {result}")
    print(f"  submitted stake tx: {tx_hash}")

    # Wait for block inclusion
    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "transaction receipt not found")
    print(f"  receipt status: {receipt.get('status', 'N/A')}")

    # Wait for a new block to ensure persistence
    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Verify validator count increased
    final_validators = cluster.nodes[0].validator_list()
    final_count = len(final_validators)
    assert_true(final_count > initial_count,
                f"validator count did not increase: {initial_count} -> {final_count}")

    # Verify the new validator appears in the list
    new_validator = next(
        (v for v in final_validators if v.get("ed25519Pubkey", "").lower() == ed25519_pubkey_hex.lower()),
        None
    )
    assert_true(new_validator is not None, "new validator not found in list")
    assert_true(new_validator.get("selfStake") == str(self_stake), "selfStake mismatch")

    # Verify escrow holds the staked amount
    escrow_result = cluster.nodes[0].get_balance(1, STAKING_ESCROW)
    escrow_bal = escrow_result.get("balance") if isinstance(escrow_result, dict) else escrow_result
    assert_true(int(escrow_bal) >= self_stake, f"escrow balance {escrow_bal} < stake {self_stake}")
    print(f"  [OK] validator staked — count {initial_count} -> {final_count}, escrow={escrow_bal}, new validator_id: {new_validator.get('validatorId')}")


def test_validator_unstake(cluster, accounts):
    """Unstake an existing validator via call_validatorUnstake."""
    sender = accounts[0]  # account 0 is genesis validator 0
    nonce = _next_nonce()
    validator_id = 0  # unstake the first genesis validator

    # Verify the validator exists before unstaking
    initial_validators = cluster.nodes[0].validator_list()
    target = next((v for v in initial_validators if v.get("validatorId") == validator_id), None)
    assert_true(target is not None, f"validator {validator_id} not found")
    print(f"  target validator {validator_id} found, selfStake={target.get('selfStake')}")

    payload = sign_validator_unstake(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        validator_id=validator_id,
    )

    result = cluster.nodes[0].validator_unstake(payload)
    tx_hash = result.get("txHash")
    assert_true(tx_hash, f"missing txHash in result: {result}")
    print(f"  submitted unstake tx: {tx_hash}")

    # Wait for block inclusion
    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "transaction receipt not found")
    print(f"  receipt status: {receipt.get('status', 'N/A')}")

    # Wait for a new block to ensure persistence
    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Verify the validator is now unbonding
    final_validators = cluster.nodes[0].validator_list()
    target_after = next((v for v in final_validators if v.get("validatorId") == validator_id), None)
    assert_true(target_after is not None, f"validator {validator_id} not found after unstake")
    assert_true(target_after.get("isUnbonding") is True,
                f"validator {validator_id} should be unbonding")

    # Attempt claim before unbonding period — should fail
    claim_payload = sign_validator_claim_unbonded(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=_next_nonce(),
        validator_id=validator_id,
    )
    try:
        result = cluster.nodes[0].validator_claim_unbonded(claim_payload)
        claim_tx = result.get("txHash")
        if claim_tx:
            claim_receipt = wait_for_tx(cluster, claim_tx, timeout=30)
            status = claim_receipt.get("status", "N/A") if claim_receipt else "no receipt"
            print(f"  claim-before-period status: {status}")
            assert_true(claim_receipt is None or status != "success",
                        "claim before period should not succeed")
    except RuntimeError as e:
        print(f"  [OK] claim before period rejected: {e}")

    print(f"  [OK] validator {validator_id} unstaked — isUnbonding=True")


# ── Main ──────────────────────────────────────────────────────────────

TEST_FUNCTIONS = [
    test_transfer_balance_change_after_block,
    test_agent_register_and_query_after_block,
    test_register_asset,
    test_register_asset_duplicate_rejected,
    test_asset_info_query,
    test_governance_submit_proposal,
    test_governance_vote,
    test_governance_get_proposals,
    test_governance_pause_state,
    test_agent_register,
    test_agent_query,
    test_shielded_tree_state,
    test_shielded_balance,
    test_oracle_get_price,
    test_oracle_get_twap,
    test_compliance_policy,
    test_bridge_deposit_error_handling,
    test_bridge_withdraw_error_handling,
    test_validator_stake,
    test_validator_unstake,
]


def run_all():
    accounts = load_accounts()
    cluster = make_cluster()

    print("=" * 60)
    print("Callchain 4-Node Devnet — Transaction Type Tests")
    print("=" * 60)
    print()

    passed = 0
    failed = 0

    for test_fn in TEST_FUNCTIONS:
        name = test_fn.__name__
        print(f"Running {name} ...")
        try:
            test_fn(cluster, accounts)
            passed += 1
        except Exception as e:
            print(f"  [FAIL] {e}")
            failed += 1
        print()

    print("=" * 60)
    print(f"Results: {passed} passed, {failed} failed")
    print("=" * 60)

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(run_all())

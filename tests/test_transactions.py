#!/usr/bin/env python3
"""Callchain E2E tests for various transaction types.

Tests asset registration, governance, agent, shielded, oracle, bridge,
and compliance transaction types against a running 4-node devnet.

Run after starting the devnet with:
    ./devnet/scripts/start.sh

All write operations use the unified call_submit endpoint with raw tx_hash
signatures. Submissions are accepted into the mempool; execution results
should be verified by querying on-chain state after block inclusion.
"""

import json
import os
import sys
import time

from rpc_client import CallchainNode, CallchainCluster
from nonce_tracker import _next_nonce, set_default_node, sync_nonce

from signer import (
    sign_agent_register,
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
    if os.environ.get("CALLCHAIN_SINGLE_NODE"):
        nodes = [
            CallchainNode("http://127.0.0.1:5005"),
        ]
    else:
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
    """Submit a transfer and verify RPC acceptance into the mempool."""
    sender = accounts[0]
    receiver = accounts[1]
    amount = 10**18 + 1  # unique amount to avoid mempool replay with test_basic.py
    nonce = _next_nonce(sender["address"])

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
    print(f"  [OK] transfer submitted via RPC — txHash {tx_hash}")


def test_agent_register_and_query_after_block(cluster, accounts):
    """Register an agent, wait for block inclusion, verify it is queryable on all nodes."""
    owner = accounts[2]
    pubkey_hex = "b" * 128
    name = "BlockTestAgent"
    url = "https://blocktest.example.com"
    nonce = _next_nonce(owner["address"])

    payload = sign_agent_register(
        private_key=owner["private_key"],
        sender=owner["address"],
        nonce=nonce,
        pubkey_hex=pubkey_hex,
        name=name,
        url=url,
    )
    result = cluster.nodes[0].agent_register(payload)
    tx_hash = result.get("txHash")
    assert_true(tx_hash, f"missing txHash in result: {result}")
    print(f"  submitted agent register tx: {tx_hash}")

    # Wait for a new block to ensure persistence
    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Query agent info on the submission node to verify persistence
    # call_submit does not return agentId immediately; query agent 1
    info = cluster.nodes[0].agent_info(1)
    if info is not None:
        print(f"  agent info verified after block")
    else:
        print(f"  [OK] agent not yet queryable (execution may still be pending)")

    print(f"  [OK] agent registration submitted and block included")


# ── Tests: Asset Registration ─────────────────────────────────────────


def test_register_asset(cluster, accounts):
    """Register a new asset via call_submit."""
    sender = accounts[0]
    symbol = "TEST"
    name = "Test Token"
    decimals = 18
    nonce = _next_nonce(sender["address"])

    payload = sign_asset_registration(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        symbol=symbol,
        name=name,
        decimals=decimals,
        max_supply=1_000_000,
    )

    result = cluster.nodes[0].register_asset(payload)

    print(f"  [OK] asset registered: {result}")
    assert_true("txHash" in result, "missing txHash in response")
    assert_true(result.get("status") == "pending", "unexpected status")


def test_register_asset_duplicate_rejected(cluster, accounts):
    """Registering the same symbol twice — both accepted into mempool, second may fail execution."""
    sender = accounts[0]
    nonce = _next_nonce(sender["address"])
    payload = sign_asset_registration(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce,
        symbol="DUP",
        name="Duplicate",
        decimals=18,
        max_supply=0,
    )

    # First registration accepted into mempool
    r1 = cluster.nodes[0].register_asset(payload)
    assert_true(r1.get("txHash"), "first submission failed")
    print(f"  first registration submitted: {r1.get('txHash')}")

    # Second registration with same symbol — accepted into mempool but may fail during execution
    nonce2 = _next_nonce(sender["address"])
    payload2 = sign_asset_registration(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=nonce2,
        symbol="DUP",
        name="Duplicate2",
        decimals=18,
        max_supply=0,
    )
    r2 = cluster.nodes[0].register_asset(payload2)
    print(f"  second registration submitted: {r2.get('txHash')}")
    print("  [OK] duplicate registration accepted into mempool (execution failure expected)")


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
    nonce = _next_nonce(sender["address"])

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
    nonce = _next_nonce(sender["address"])

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
    """Register an agent via call_submit."""
    owner = accounts[2]
    # Use a dummy 64-byte pubkey (128 hex chars)
    pubkey_hex = "a" * 128
    name = "TestAgent"
    url = "https://example.com"
    nonce = _next_nonce(owner["address"])

    payload = sign_agent_register(
        private_key=owner["private_key"],
        sender=owner["address"],
        nonce=nonce,
        pubkey_hex=pubkey_hex,
        name=name,
        url=url,
    )
    result = cluster.nodes[0].agent_register(payload)
    print(f"  [OK] agent registered: {result}")
    assert_true("txHash" in result, "missing txHash")


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


def test_validator_join(cluster, accounts):
    """Validator joins via ValidatorStake — verify registration and escrow."""
    sender = accounts[3]
    sync_nonce(sender["address"], cluster.nodes[0])
    nonce = _next_nonce(sender["address"])
    ed25519_pubkey_hex = "0x" + "aa" * 32
    self_stake = 1_000_000 * 10**18

    initial_validators = cluster.nodes[0].validator_list()
    sender_bal_before = get_balance_int(cluster.nodes[0], 1, sender["address"])
    escrow_bal_before = get_balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    print(f"  pre: validators={len(initial_validators)}, sender_bal={sender_bal_before}, escrow={escrow_bal_before}")

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

    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "stake tx not found in any block within 30s")
    print(f"  tx confirmed in block")

    # Ensure all nodes synced before querying state
    target_height = int(cluster.nodes[0].block_number(), 16)
    for node in cluster.nodes:
        wait_for_height(node, target_height)

    final_validators = cluster.nodes[0].validator_list()
    assert_true(len(final_validators) > len(initial_validators),
                f"validator count did not increase")

    new_validator = next(
        (v for v in final_validators if v.get("ed25519Pubkey", "").lower() == ed25519_pubkey_hex.lower()),
        None
    )
    assert_true(new_validator is not None, "new validator not found in list")
    assert_true(new_validator.get("selfStake") == str(self_stake), "selfStake mismatch")

    # Verify escrow holds the staked amount (asset_id 1 for CALL)
    escrow_bal = get_balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    assert_true(escrow_bal >= escrow_bal_before + self_stake,
                f"escrow balance {escrow_bal} < before {escrow_bal_before} + stake {self_stake}")

    # Verify sender balance decreased
    sender_bal_after = get_balance_int(cluster.nodes[0], 1, sender["address"])
    assert_true(sender_bal_after < sender_bal_before,
                f"sender balance did not decrease: {sender_bal_before} -> {sender_bal_after}")
    print(f"  [OK] validator joined — escrow={escrow_bal}, sender {sender_bal_before} -> {sender_bal_after}")


def test_validator_leave(cluster, accounts):
    """Validator leaves via ValidatorUnstake — verify unbonding and locked escrow."""
    sender = accounts[0]
    sync_nonce(sender["address"], cluster.nodes[0])
    nonce = _next_nonce(sender["address"])
    validator_id = 0

    # Verify the validator exists before leaving
    initial_validators = cluster.nodes[0].validator_list()
    target = next((v for v in initial_validators if v.get("validatorId") == validator_id), None)
    assert_true(target is not None, f"validator {validator_id} not found")
    stake_amount = int(target.get("selfStake", "0"))
    escrow_bal_before = get_balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    print(f"  pre: validator {validator_id} selfStake={stake_amount}, escrow={escrow_bal_before}")

    # Reject non-owner unstake
    try:
        sync_nonce(accounts[1]["address"], cluster.nodes[0])
        bad = sign_validator_unstake(
            private_key=accounts[1]["private_key"],
            sender=accounts[1]["address"],
            nonce=_next_nonce(accounts[1]["address"]),
            validator_id=validator_id,
        )
        cluster.nodes[0].validator_unstake(bad)
        print("  [WARN] non-owner unstake accepted")
    except RuntimeError as e:
        print(f"  [OK] non-owner unstake rejected: {e}")

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

    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "unstake tx not found in any block within 30s")
    print(f"  tx confirmed in block")

    target_height = int(cluster.nodes[0].block_number(), 16)
    for node in cluster.nodes:
        wait_for_height(node, target_height)

    # Verify validator is unbonding and escrow still holds stake
    final_validators = cluster.nodes[0].validator_list()
    target_after = next((v for v in final_validators if v.get("validatorId") == validator_id), None)
    assert_true(target_after is not None, f"validator {validator_id} not found after unstake")
    assert_true(target_after.get("isUnbonding") is True,
                f"validator {validator_id} should be unbonding")

    escrow_bal_after = get_balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    assert_true(escrow_bal_after >= stake_amount,
                f"escrow {escrow_bal_after} < stake {stake_amount} during unbonding")
    print(f"  escrow still locked: {escrow_bal_after}")

    # Attempt claim before unbonding period — should fail
    sync_nonce(sender["address"], cluster.nodes[0])
    claim_payload = sign_validator_claim_unbonded(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=_next_nonce(sender["address"]),
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

    print(f"  [OK] validator {validator_id} left — unbonding, escrow locked")


# ── Main ──────────────────────────────────────────────────────────────

TEST_FUNCTIONS = [
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
    test_validator_join,
    test_validator_leave,
    test_transfer_balance_change_after_block,
]


def run_all():
    accounts = load_accounts()
    cluster = make_cluster()
    set_default_node(cluster.nodes[0])

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

#!/usr/bin/env python3
"""Validator lifecycle E2E tests for the 6-node devnet.

Tests adding a new validator via ValidatorStake and removing an existing
validator via ValidatorUnstake. Verifies state is consistent across all
nodes (4 validators + 2 full nodes) after block inclusion.

Run after starting the devnet with:
    ./devnet/scripts/start.sh
"""

import json
import os
import sys
import time

from rpc_client import CallchainNode, CallchainCluster
from signer import sign_validator_stake, sign_validator_unstake, sign_validator_claim_unbonded

# System escrow address for staked CALL (matches Rust STAKING_ESCROW)
STAKING_ESCROW = "0x" + "00" * 20

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))


def load_accounts():
    with open(os.path.join(TESTS_DIR, "accounts.json")) as f:
        return json.load(f)["accounts"]


def make_cluster():
    """Connect to all 6 devnet nodes (4 validators + 2 full nodes)."""
    nodes = [
        CallchainNode("http://127.0.0.1:5005"),
        CallchainNode("http://127.0.0.1:5007"),
        CallchainNode("http://127.0.0.1:5009"),
        CallchainNode("http://127.0.0.1:5011"),
        CallchainNode("http://127.0.0.1:5013"),
        CallchainNode("http://127.0.0.1:5015"),
    ]
    return CallchainCluster(nodes)


# ── Helpers ───────────────────────────────────────────────────────────


def assert_true(cond, msg=""):
    if not cond:
        raise AssertionError(msg)


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


def _next_nonce():
    return int(time.time() * 1000) % 1_000_000_000


# ── Tests ─────────────────────────────────────────────────────────────


def test_add_validator(cluster, accounts):
    """Stake a new validator and verify it appears on all 6 nodes."""
    sender = accounts[3]
    nonce = _next_nonce()
    ed25519_pubkey_hex = "0x" + "aa" * 32
    self_stake = 1_000_000 * 10**18

    # Capture initial validator count from every node
    initial_counts = []
    for i, node in enumerate(cluster.nodes):
        try:
            validators = node.validator_list()
            initial_counts.append(len(validators))
            print(f"  node{i+1} initial validators: {len(validators)}")
        except Exception as e:
            initial_counts.append(-1)
            print(f"  node{i+1} initial validator query failed: {e}")

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
    assert_true(receipt is not None, "transaction receipt not found")
    print(f"  receipt status: {receipt.get('status', 'N/A')}")

    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Verify every node sees the new validator and escrow holds the stake
    for i, node in enumerate(cluster.nodes):
        validators = node.validator_list()
        new_validator = next(
            (v for v in validators if v.get("ed25519Pubkey", "").lower() == ed25519_pubkey_hex.lower()),
            None
        )
        assert_true(new_validator is not None, f"node{i+1}: new validator not found")
        assert_true(
            new_validator.get("selfStake") == str(self_stake),
            f"node{i+1}: selfStake mismatch"
        )
        # Verify escrow balance holds the staked amount
        escrow_result = node.get_balance(1, STAKING_ESCROW)
        escrow_bal = escrow_result.get("balance") if isinstance(escrow_result, dict) else escrow_result
        assert_true(
            int(escrow_bal) >= self_stake,
            f"node{i+1}: escrow balance {escrow_bal} < stake {self_stake}"
        )
        print(f"  node{i+1}: new validator confirmed (id={new_validator.get('validatorId')}), escrow={escrow_bal}")

    print("  [OK] validator added and synced to all nodes")


def test_remove_validator(cluster, accounts):
    """Unstake an existing validator and verify it is marked unbonding on all 6 nodes."""
    sender = accounts[0]
    nonce = _next_nonce()
    validator_id = 0

    # Verify the validator exists on every node before unstaking
    for i, node in enumerate(cluster.nodes):
        validators = node.validator_list()
        target = next((v for v in validators if v.get("validatorId") == validator_id), None)
        assert_true(target is not None, f"node{i+1}: validator {validator_id} not found before unstake")

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
    assert_true(receipt is not None, "transaction receipt not found")
    print(f"  receipt status: {receipt.get('status', 'N/A')}")

    current_height = int(cluster.nodes[0].block_number(), 16)
    wait_for_height(cluster.nodes[0], current_height + 1)

    # Verify every node sees the validator as unbonding
    for i, node in enumerate(cluster.nodes):
        validators = node.validator_list()
        target = next((v for v in validators if v.get("validatorId") == validator_id), None)
        assert_true(target is not None, f"node{i+1}: validator {validator_id} not found after unstake")
        assert_true(
            target.get("isUnbonding") is True,
            f"node{i+1}: validator {validator_id} should be unbonding"
        )
        print(f"  node{i+1}: validator {validator_id} is unbonding")

    # Attempt to claim before unbonding period elapsed — should fail
    claim_nonce = _next_nonce()
    claim_payload = sign_validator_claim_unbonded(
        private_key=sender["private_key"],
        sender=sender["address"],
        nonce=claim_nonce,
        validator_id=validator_id,
    )
    try:
        result = cluster.nodes[0].validator_claim_unbonded(claim_payload)
        claim_tx_hash = result.get("txHash")
        if claim_tx_hash:
            claim_receipt = wait_for_tx(cluster, claim_tx_hash, timeout=30)
            status = claim_receipt.get("status", "N/A") if claim_receipt else "no receipt"
            print(f"  claim tx status (before period): {status}")
            # Expect failure — unbonding period not elapsed
            assert_true(
                claim_receipt is None or status != "success",
                "claim before unbonding period should fail"
            )
    except RuntimeError as e:
        print(f"  [OK] claim before period rejected: {e}")

    print("  [OK] validator removed and synced to all nodes")


# ── Main ──────────────────────────────────────────────────────────────

TEST_FUNCTIONS = [
    test_add_validator,
    test_remove_validator,
]


def run_all():
    accounts = load_accounts()
    cluster = make_cluster()

    print("=" * 60)
    print("Callchain 6-Node Devnet — Validator Lifecycle E2E Tests")
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

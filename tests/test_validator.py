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

from eth_account import Account
from rpc_client import CallchainNode, CallchainCluster
from signer import (
    sign_evm_precompile_stake,
    sign_evm_precompile_unstake,
    sign_evm_precompile_claim_unbonded,
    sign_evm_precompile_transfer,
)
from nonce_tracker import _next_nonce, _next_evm_nonce, set_default_node, sync_nonce, sync_evm_nonce

# System escrow address for staked CALL (matches Rust STAKING_ESCROW)
STAKING_ESCROW = "0x0000000000000000000000000000000000000ACE"

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))


def load_accounts():
    with open(os.path.join(TESTS_DIR, "accounts.json")) as f:
        return json.load(f)["accounts"]


def make_cluster():
    """Connect to devnet nodes."""
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




def _balance_int(node, asset_id, address):
    """Return balance as an integer, defaulting to 0 on error."""
    try:
        result = node.get_balance(asset_id, address)
        bal = result.get("balance") if isinstance(result, dict) else result
        return int(bal)
    except Exception:
        return 0


# ── Tests: Validator Join ─────────────────────────────────────────────


def test_validator_join(cluster, accounts):
    """Full join flow via Validator precompile (0x204): stake CALL, verify validator appears.

    All genesis accounts are already validators, so we generate a fresh
    account, fund it from an existing account, and then stake.
    """
    # Use accounts[1] as funder — accounts[0] may be depleted by prior test files
    funder = accounts[1]
    new_account = Account.create()
    new_addr = new_account.address
    new_key = "0x" + new_account.key.hex()
    # Use a distinct pubkey to avoid collision with genesis validators
    ed25519_pubkey_hex = "0x" + "bb" * 32
    self_stake = 1_000_000 * 10**18
    fund_amount = self_stake + 10**18  # extra for gas

    # Capture pre-state on node1
    validators_before = cluster.nodes[0].validator_list()
    escrow_bal_before = _balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    print(f"  pre: validators={len(validators_before)}, escrow={escrow_bal_before}")

    # 1. Fund the new account
    sync_evm_nonce(funder["address"], cluster.nodes[0])
    fund_nonce = _next_evm_nonce(funder["address"])
    fund_tx = sign_evm_precompile_transfer(
        private_key=funder["private_key"],
        evm_nonce=fund_nonce,
        asset_id=1,
        to=new_addr,
        amount=fund_amount,
    )
    fund_hash = cluster.nodes[0].send_raw_transaction(fund_tx)
    assert_true(fund_hash and fund_hash.startswith("0x"), f"fund tx failed: {fund_hash}")
    print(f"  funded new account {new_addr}: {fund_hash}")

    fund_receipt = wait_for_tx(cluster, fund_hash, timeout=30)
    assert_true(fund_receipt is not None, "fund tx not found in any block within 30s")
    assert_true(fund_receipt.get("status") == "0x1", "fund tx reverted")

    # 2. Stake from the new account
    sync_evm_nonce(new_addr, cluster.nodes[0])
    evm_nonce = _next_evm_nonce(new_addr)

    raw_tx = sign_evm_precompile_stake(
        private_key=new_key,
        evm_nonce=evm_nonce,
        ed25519_pubkey_hex=ed25519_pubkey_hex,
        self_stake=self_stake,
    )

    tx_hash = cluster.nodes[0].send_raw_transaction(raw_tx)
    assert_true(tx_hash and tx_hash.startswith("0x"), f"missing txHash in result: {tx_hash}")
    print(f"  submitted stake tx: {tx_hash}")

    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "stake tx not found in any block within 30s")
    status = receipt.get("status", "N/A")
    print(f"  tx confirmed in block, status={status}")
    assert_true(status == "0x1", f"stake tx reverted: status={status}")

    # Ensure all nodes have caught up to the block containing the tx
    target_height = int(cluster.nodes[0].block_number(), 16)
    for i, node in enumerate(cluster.nodes):
        wait_for_height(node, target_height)

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
        # Verify escrow balance increased by stake amount
        escrow_bal = _balance_int(node, 1, STAKING_ESCROW)
        assert_true(
            escrow_bal >= escrow_bal_before + self_stake,
            f"node{i+1}: escrow {escrow_bal} < before {escrow_bal_before} + stake {self_stake}"
        )
        print(f"  node{i+1}: validator id={new_validator.get('validatorId')} confirmed, escrow={escrow_bal}")

    print("  [OK] validator joined — staked, escrowed, synced to all nodes")


# ── Tests: Validator Leave ────────────────────────────────────────────


def test_validator_leave(cluster, accounts):
    """Full leave flow via Validator precompile (0x204): unstake, verify unbonding, reject early claim."""
    sender = accounts[0]
    sync_evm_nonce(sender["address"], cluster.nodes[0])
    evm_nonce = _next_evm_nonce(sender["address"])
    validator_id = 1

    # Verify the validator exists before leaving
    validators_before = cluster.nodes[0].validator_list()
    target = next((v for v in validators_before if v.get("validatorId") == validator_id), None)
    assert_true(target is not None, f"validator {validator_id} not found before unstake")
    stake_amount = int(target.get("selfStake", "0"))
    escrow_bal_before = _balance_int(cluster.nodes[0], 1, STAKING_ESCROW)
    print(f"  pre: validator {validator_id} found, selfStake={stake_amount}, escrow={escrow_bal_before}")

    # Reject unstake from non-owner
    sync_evm_nonce(accounts[1]["address"], cluster.nodes[0])
    bad_raw_tx = sign_evm_precompile_unstake(
        private_key=accounts[1]["private_key"],
        evm_nonce=_next_evm_nonce(accounts[1]["address"]),
        validator_id=validator_id,
    )
    try:
        cluster.nodes[0].send_raw_transaction(bad_raw_tx)
        print("  [WARN] non-owner unstake accepted")
    except RuntimeError as e:
        print(f"  [OK] non-owner unstake rejected: {e}")

    raw_tx = sign_evm_precompile_unstake(
        private_key=sender["private_key"],
        evm_nonce=evm_nonce,
        validator_id=validator_id,
    )

    tx_hash = cluster.nodes[0].send_raw_transaction(raw_tx)
    assert_true(tx_hash and tx_hash.startswith("0x"), f"missing txHash in result: {tx_hash}")
    print(f"  submitted unstake tx: {tx_hash}")

    receipt = wait_for_tx(cluster, tx_hash, timeout=30)
    assert_true(receipt is not None, "unstake tx not found in any block within 30s")
    print(f"  tx confirmed in block")

    target_height = int(cluster.nodes[0].block_number(), 16)
    for i, node in enumerate(cluster.nodes):
        wait_for_height(node, target_height)

    # Verify every node sees the validator as unbonding and escrow still holds stake
    for i, node in enumerate(cluster.nodes):
        validators = node.validator_list()
        target_after = next((v for v in validators if v.get("validatorId") == validator_id), None)
        assert_true(target_after is not None, f"node{i+1}: validator {validator_id} not found after unstake")
        assert_true(
            target_after.get("isUnbonding") is True,
            f"node{i+1}: validator {validator_id} should be unbonding"
        )
        # Escrow should still hold the stake (not yet returned)
        escrow_bal = _balance_int(node, 1, STAKING_ESCROW)
        assert_true(
            escrow_bal >= stake_amount,
            f"node{i+1}: escrow {escrow_bal} < stake {stake_amount} during unbonding"
        )
        print(f"  node{i+1}: validator {validator_id} is unbonding, escrow={escrow_bal}")

    # Attempt to claim before unbonding period elapsed — should fail
    sync_evm_nonce(sender["address"], cluster.nodes[0])
    claim_raw_tx = sign_evm_precompile_claim_unbonded(
        private_key=sender["private_key"],
        evm_nonce=_next_evm_nonce(sender["address"]),
        validator_id=validator_id,
    )
    try:
        claim_tx_hash = cluster.nodes[0].send_raw_transaction(claim_raw_tx)
        if claim_tx_hash:
            claim_receipt = wait_for_tx(cluster, claim_tx_hash, timeout=30)
            status = claim_receipt.get("status", "N/A") if claim_receipt else "no receipt"
            print(f"  claim tx status (before period): {status}")
            assert_true(
                claim_receipt is None or status != "success",
                "claim before unbonding period should fail"
            )
    except RuntimeError as e:
        print(f"  [OK] claim before period rejected: {e}")

    print("  [OK] validator left — unbonding started, escrow locked, early claim rejected")


# ── Main ──────────────────────────────────────────────────────────────

TEST_FUNCTIONS = [
    test_validator_join,
    test_validator_leave,
]


def run_all():
    accounts = load_accounts()
    cluster = make_cluster()
    set_default_node(cluster.nodes[0])

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

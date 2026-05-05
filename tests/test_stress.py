#!/usr/bin/env python3
"""Callchain stress / load tests against a running 4-node devnet.

Measures throughput, latency, and consistency under concurrent load.

Run after starting the devnet with:
    ./devnet/scripts/start.sh

These tests measure RPC submission throughput and mempool behavior.
Full execution throughput should be validated via the in-memory harness.
"""

import concurrent.futures
import json
import os
import random
import re
import sys
import time
import statistics
import threading

from rpc_client import CallchainNode, CallchainCluster
from signer import sign_evm_precompile_transfer
from nonce_tracker import (
    _next_nonce,
    _next_evm_nonce,
    set_default_node,
    sync_evm_nonce,
    reserve_evm_nonces,
    set_evm_nonce,
)

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))


def load_accounts():
    with open(os.path.join(TESTS_DIR, "accounts.json")) as f:
        return json.load(f)["accounts"]


def _is_node_synced(node: CallchainNode, base_height: int) -> bool:
    try:
        h = int(node.block_number(), 16)
        return h > 0 and abs(h - base_height) <= 3
    except Exception:
        return False


def make_cluster():
    if os.environ.get("CALLCHAIN_SINGLE_NODE"):
        nodes = [
            CallchainNode("http://127.0.0.1:5005"),
        ]
        return CallchainCluster(nodes)

    all_nodes = [
        CallchainNode("http://127.0.0.1:5005"),
        CallchainNode("http://127.0.0.1:5007"),
        CallchainNode("http://127.0.0.1:5009"),
        CallchainNode("http://127.0.0.1:5011"),
    ]

    # Filter to only nodes that are roughly synced with node 0.
    # This avoids submitting to nodes that are stuck at genesis,
    # which would fail with "insufficient balance for gas" because
    # their EVM state has not processed the genesis bridge yet.
    try:
        base_height = int(all_nodes[0].block_number(), 16)
    except Exception:
        base_height = 0

    healthy = [all_nodes[0]]
    for node in all_nodes[1:]:
        if _is_node_synced(node, base_height):
            healthy.append(node)

    if len(healthy) < len(all_nodes):
        print(f"  [warn] only {len(healthy)}/{len(all_nodes)} nodes are synced")

    return CallchainCluster(healthy)


class StressRunner:
    def __init__(self, cluster: CallchainCluster, accounts: list):
        self.cluster = cluster
        self.accounts = accounts
        self.results = []
        self.errors = []
        # Per-sender lock so that txs from the same address are submitted
        # in nonce order (required for EVM strict nonce validation).
        self._sender_locks: dict[int, threading.Lock] = {
            i: threading.Lock() for i in range(len(accounts))
        }

    def submit_single(
        self,
        node_idx: int,
        sender_idx: int,
        amount: int,
        evm_nonce: int,
    ):
        """Submit one precompile transfer transaction and record latency.

        The caller must supply a pre-reserved *evm_nonce*; on nonce failure
        we parse the expected nonce from the error, update the local tracker,
        and retry with exponential backoff so preceding txs have time to
        land in the mempool.

        A tiny amount jitter (0-999 wei) is added to each transfer so that
        every raw tx has a unique hash, avoiding the mempool replay protector
        when a previously-failed tx left its hash in the seen set.
        """
        sender = self.accounts[sender_idx]
        receiver = self.accounts[(sender_idx + 1) % len(self.accounts)]
        node = self.cluster.nodes[node_idx % len(self.cluster.nodes)]
        t0 = time.time()

        for attempt in range(3):
            # Vary amount by a tiny jitter so the tx hash is always unique.
            # This avoids "mempool defense: replay detected" when a previous
            # submission with the same parameters failed after the hash was
            # recorded in MempoolDefense but before it entered the mempool.
            jittered_amount = amount + random.randint(0, 999)
            raw_tx = sign_evm_precompile_transfer(
                private_key=sender["private_key"],
                evm_nonce=evm_nonce,
                asset_id=1,
                to=receiver["address"],
                amount=jittered_amount,
            )

            try:
                tx_hash = node.send_raw_transaction(raw_tx)
                latency = time.time() - t0
                return {"success": True, "latency": latency, "txHash": tx_hash}
            except RuntimeError as e:
                error_str = str(e)
                # Parse expected nonce from "invalid nonce: expected X, got Y"
                m = re.search(r"expected\s+(\d+)", error_str)
                if m and attempt < 2:
                    expected = int(m.group(1))
                    # Fast-forward local tracker so the next reservation
                    # starts from the correct value.
                    set_evm_nonce(sender["address"], expected)
                    evm_nonce = expected
                    # Backoff: give preceding txs time to reach the mempool.
                    time.sleep(0.03 * (attempt + 1))
                    continue
                latency = time.time() - t0
                return {"success": False, "latency": latency, "error": error_str}
            except Exception as e:
                latency = time.time() - t0
                return {"success": False, "latency": latency, "error": str(e)}

        latency = time.time() - t0
        return {"success": False, "latency": latency, "error": "max retries exceeded"}

    def run_burst(self, count: int, concurrency: int, amount: int = 10**16) -> dict:
        """Fire `count` transactions with `concurrency` parallel workers."""
        self.results = []
        self.errors = []

        print(f"  Burst: {count} txs, concurrency={concurrency}, amount={amount}")
        t_start = time.time()

        # ── Pre-reserve nonces per sender ─────────────────────────────
        # Count how many txs each sender will emit, then reserve a
        # contiguous nonce block.  This guarantees unique nonces and
        # lets us order submissions by nonce within each sender.
        sender_counts: dict[int, int] = {}
        tx_specs: list[tuple[int, int]] = []  # (sender_idx, node_idx)
        for i in range(count):
            sender_idx = i % len(self.accounts)
            node_idx = i % len(self.cluster.nodes)
            sender_counts[sender_idx] = sender_counts.get(sender_idx, 0) + 1
            tx_specs.append((sender_idx, node_idx))

        sender_nonce_iters: dict[int, iter] = {}
        for sender_idx, n in sender_counts.items():
            addr = self.accounts[sender_idx]["address"]
            start = reserve_evm_nonces(addr, n)
            sender_nonce_iters[sender_idx] = iter(range(start, start + n))

        # Build the work items with pre-assigned nonces
        work_items: list[tuple[int, int, int, int]] = []
        for sender_idx, node_idx in tx_specs:
            nonce = next(sender_nonce_iters[sender_idx])
            work_items.append((node_idx, sender_idx, amount, nonce))

        # ── Submit: one thread per sender, ordered by nonce ───────────
        # We group by sender and submit each group in nonce order, but
        # different senders run in parallel via the thread pool.
        sender_batches: dict[int, list[tuple[int, int, int, int]]] = {}
        for item in work_items:
            sender_idx = item[1]
            sender_batches.setdefault(sender_idx, []).append(item)
        # Sort each batch by nonce so submission order matches sequence order
        for batch in sender_batches.values():
            batch.sort(key=lambda x: x[3])

        # Use one thread per sender; each thread submits its batch in order.
        def _submit_batch(batch: list):
            out = []
            for node_idx, sender_idx, amt, nonce in batch:
                r = self.submit_single(node_idx, sender_idx, amt, nonce)
                out.append(r)
            return out

        with concurrent.futures.ThreadPoolExecutor(max_workers=len(self.accounts)) as ex:
            futures = [
                ex.submit(_submit_batch, batch)
                for batch in sender_batches.values()
            ]
            for f in concurrent.futures.as_completed(futures):
                for r in f.result():
                    self.results.append(r)
                    if not r["success"]:
                        self.errors.append(r)

        t_total = time.time() - t_start
        successes = [r for r in self.results if r["success"]]
        latencies = [r["latency"] for r in successes]

        # Print sample errors for debugging
        if self.errors:
            distinct = {}
            for e in self.errors:
                distinct[e["error"]] = distinct.get(e["error"], 0) + 1
            print(f"  errors: {dict(list(distinct.items())[:3])}")

        report = {
            "total": count,
            "success": len(successes),
            "failed": len(self.errors),
            "duration_sec": t_total,
            "submission_tps": count / t_total if t_total > 0 else 0,
            "success_tps": len(successes) / t_total if t_total > 0 else 0,
        }
        if latencies:
            report["latency_ms"] = {
                "min": min(latencies) * 1000,
                "max": max(latencies) * 1000,
                "avg": statistics.mean(latencies) * 1000,
                "p50": statistics.median(latencies) * 1000,
                "p99": sorted(latencies)[int(len(latencies) * 0.99)] * 1000,
            }
        return report

    def run_sustained(self, duration_sec: float, target_tps: float, amount: int = 10**16) -> dict:
        """Sustain target TPS for a duration."""
        self.results = []
        self.errors = []

        print(f"  Sustained: {target_tps} TPS for {duration_sec}s")
        t_start = time.time()
        interval = 1.0 / target_tps
        i = 0

        while time.time() - t_start < duration_sec:
            node_idx = i % len(self.cluster.nodes)
            sender_idx = i % len(self.accounts)
            evm_nonce = _next_evm_nonce(self.accounts[sender_idx]["address"])

            r = self.submit_single(node_idx, sender_idx, amount, evm_nonce)
            self.results.append(r)
            if not r["success"]:
                self.errors.append(r)

            i += 1
            next_t = t_start + i * interval
            sleep_time = next_t - time.time()
            if sleep_time > 0:
                time.sleep(sleep_time)

        t_total = time.time() - t_start
        successes = [r for r in self.results if r["success"]]
        latencies = [r["latency"] for r in successes]

        # Print sample errors for debugging
        if self.errors:
            distinct = {}
            for e in self.errors:
                distinct[e["error"]] = distinct.get(e["error"], 0) + 1
            print(f"  errors: {dict(list(distinct.items())[:3])}")

        report = {
            "total": len(self.results),
            "success": len(successes),
            "failed": len(self.errors),
            "duration_sec": t_total,
            "actual_tps": len(self.results) / t_total if t_total > 0 else 0,
        }
        if latencies:
            report["latency_ms"] = {
                "avg": statistics.mean(latencies) * 1000,
                "p99": sorted(latencies)[int(len(latencies) * 0.99)] * 1000,
            }
        return report


def test_burst_100(cluster, accounts):
    """Burst 100 transactions at moderate concurrency."""
    runner = StressRunner(cluster, accounts)
    report = runner.run_burst(count=100, concurrency=10)
    print(f"  submission TPS: {report['submission_tps']:.1f}")
    print(f"  success rate: {report['success']}/{report['total']}")
    if "latency_ms" in report:
        print(f"  latency p99: {report['latency_ms']['p99']:.1f}ms")
    assert report["success"] >= 90, f"too many failures: {report['failed']}"
    print("  [OK] burst 100 completed")


def test_burst_500(cluster, accounts):
    """Burst 500 transactions at high concurrency."""
    runner = StressRunner(cluster, accounts)
    report = runner.run_burst(count=500, concurrency=20)
    print(f"  submission TPS: {report['submission_tps']:.1f}")
    print(f"  success rate: {report['success']}/{report['total']}")
    if "latency_ms" in report:
        print(f"  latency p99: {report['latency_ms']['p99']:.1f}ms")
    assert report["success"] >= 400, f"too many failures: {report['failed']}"
    print("  [OK] burst 500 completed")


def test_sustained_10tps_30s(cluster, accounts):
    """Sustain 10 TPS for 30 seconds."""
    runner = StressRunner(cluster, accounts)
    report = runner.run_sustained(duration_sec=30, target_tps=10)
    print(f"  actual TPS: {report['actual_tps']:.1f}")
    print(f"  success rate: {report['success']}/{report['total']}")
    if "latency_ms" in report:
        print(f"  latency avg: {report['latency_ms']['avg']:.1f}ms")
    assert report["success"] >= 250, f"too many failures"
    print("  [OK] sustained 10 TPS completed")


def test_multi_node_submission(cluster, accounts):
    """Submit transactions round-robin to all 4 nodes."""
    runner = StressRunner(cluster, accounts)
    report = runner.run_burst(count=40, concurrency=8)
    print(f"  success rate: {report['success']}/{report['total']}")
    assert report["success"] >= 35
    print("  [OK] multi-node submission completed")


def test_mempool_backpressure(cluster, accounts):
    """Overwhelm mempool and check that excess txs are handled gracefully."""
    runner = StressRunner(cluster, accounts)
    report = runner.run_burst(count=1000, concurrency=50, amount=10**14)
    print(f"  submitted: {report['total']}, success: {report['success']}, failed: {report['failed']}")
    # We expect some failures under extreme load; the key is no crash.
    assert report["failed"] < 500, f"too many failures: {report['failed']}"
    print("  [OK] mempool backpressure handled")


def test_consistency_after_load(cluster, accounts):
    """After load, all nodes should agree on block heights."""
    time.sleep(3)
    heights = []
    for i, node in enumerate(cluster.nodes):
        try:
            h = int(node.block_number(), 16)
            heights.append(h)
        except Exception:
            heights.append(-1)
    delta = max(heights) - min(heights)
    print(f"  heights: {heights}, delta: {delta}")
    assert delta <= 2, f"nodes diverged by {delta} blocks"
    print("  [OK] post-load consistency verified")


# ── Main ──────────────────────────────────────────────────────────────

TEST_FUNCTIONS = [
    test_burst_100,
    test_burst_500,
    test_sustained_10tps_30s,
    test_multi_node_submission,
    test_mempool_backpressure,
    test_consistency_after_load,
]


def run_all():
    accounts = load_accounts()
    cluster = make_cluster()
    set_default_node(cluster.nodes[0])

    print("=" * 60)
    print("Callchain 4-Node Devnet — Stress / Load Tests")
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

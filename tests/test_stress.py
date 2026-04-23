#!/usr/bin/env python3
"""Callchain stress / load tests against a running 4-node devnet.

Measures throughput, latency, and consistency under concurrent load.

Run after starting the devnet with:
    ./devnet/scripts/start.sh

KNOWN LIMITATION:
See test_basic.py docstring for the RPC signature verification mismatch.
These tests measure RPC submission throughput and mempool behavior.
Full execution throughput should be validated via the in-memory harness.
"""

import concurrent.futures
import json
import os
import sys
import time
import statistics
import threading

from rpc_client import CallchainNode, CallchainCluster
from signer import sign_payment

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


class StressRunner:
    def __init__(self, cluster: CallchainCluster, accounts: list):
        self.cluster = cluster
        self.accounts = accounts
        self.results = []
        self.errors = []
        # Per-account nonces start at 0 and increment sequentially.
        self.next_nonce = [0] * len(accounts)
        self._nonce_lock = threading.Lock()

    def _take_nonce(self, sender_idx: int) -> int:
        with self._nonce_lock:
            nonce = self.next_nonce[sender_idx]
            self.next_nonce[sender_idx] += 1
            return nonce

    def submit_single(self, node_idx: int, sender_idx: int, amount: int):
        """Submit one transaction and record latency."""
        sender = self.accounts[sender_idx]
        receiver = self.accounts[(sender_idx + 1) % len(self.accounts)]
        nonce = self._take_nonce(sender_idx)

        payload = sign_payment(
            private_key=sender["private_key"],
            sender=sender["address"],
            nonce=nonce,
            asset_id=1,
            to=receiver["address"],
            amount=amount,
        )

        node = self.cluster.nodes[node_idx % len(self.cluster.nodes)]
        t0 = time.time()
        try:
            result = node.send_payment(payload)
            latency = time.time() - t0
            return {"success": True, "latency": latency, "txHash": result.get("txHash", "")}
        except Exception as e:
            latency = time.time() - t0
            return {"success": False, "latency": latency, "error": str(e)}

    def run_burst(self, count: int, concurrency: int, amount: int = 10**16) -> dict:
        """Fire `count` transactions with `concurrency` parallel workers."""
        self.results = []
        self.errors = []

        print(f"  Burst: {count} txs, concurrency={concurrency}, amount={amount}")
        t_start = time.time()

        with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as ex:
            futures = []
            for i in range(count):
                node_idx = i % 4
                sender_idx = i % len(self.accounts)
                f = ex.submit(self.submit_single, node_idx, sender_idx, amount)
                futures.append(f)

            for f in concurrent.futures.as_completed(futures):
                r = f.result()
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
            node_idx = i % 4
            sender_idx = i % len(self.accounts)

            r = self.submit_single(node_idx, sender_idx, amount)
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

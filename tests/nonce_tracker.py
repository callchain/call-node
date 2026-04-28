"""Shared per-address nonce tracker for E2E tests.

Provides `_next_nonce()` that persists state across test file invocations
via a JSON file, preventing replay errors when multiple test scripts run
sequentially against the same devnet.

When a default RPC node is set via `set_default_node()`, the tracker
queries the node's actual nonce when initialising a new counter, ensuring
counters stay in sync with on-chain state even if previous transactions
failed execution (and therefore did not consume a nonce).
"""

import json
import os
import threading

_TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
_STATE_FILE = os.path.join(_TESTS_DIR, ".nonce_state.json")
_LOCK = threading.RLock()


def _load_state():
    if os.path.exists(_STATE_FILE):
        try:
            with open(_STATE_FILE) as f:
                return json.load(f)
        except (json.JSONDecodeError, OSError):
            pass
    return {}


def _save_state():
    with _LOCK:
        payload = {"protocol": _NONCE_COUNTERS, "evm": _EVM_NONCE_COUNTERS}
        with open(_STATE_FILE, "w") as f:
            json.dump(payload, f)


# Load existing state (shared across test files in the same devnet run)
_state = _load_state()
_NONCE_COUNTERS = _state.get("protocol", {})
_EVM_NONCE_COUNTERS = _state.get("evm", {})


def set_default_node(node):
    """Set the default RPC node for nonce queries."""
    global _DEFAULT_NODE
    _DEFAULT_NODE = node


def sync_nonce(address, node=None):
    """Resync the local counter for *address* with the node's actual nonce."""
    global _NONCE_COUNTERS
    key = address.lower() if address else "__global__"
    target = node or _DEFAULT_NODE
    if target is not None:
        try:
            actual = target.get_nonce(key)
            with _LOCK:
                _NONCE_COUNTERS[key] = actual
                _save_state()
        except Exception:
            pass


def _next_nonce(address=None, node=None):
    """Return the next sequential nonce for an address.

    On first use (no local counter), queries the node for the actual nonce
    if a node is available; otherwise starts from 0.  Subsequent calls
    increment the local counter.
    """
    global _NONCE_COUNTERS
    key = address.lower() if address else "__global__"

    with _LOCK:
        if key not in _NONCE_COUNTERS:
            target = node or _DEFAULT_NODE
            if target is not None:
                try:
                    _NONCE_COUNTERS[key] = target.get_nonce(key)
                except Exception:
                    _NONCE_COUNTERS[key] = 0
            else:
                _NONCE_COUNTERS[key] = 0

        nonce = _NONCE_COUNTERS[key]
        _NONCE_COUNTERS[key] = nonce + 1
        _save_state()
        return nonce


# ── EVM nonce tracking (separate from protocol nonces) ───────────────


def sync_evm_nonce(address, node=None):
    """Resync the local EVM nonce counter for *address* with eth_getTransactionCount."""
    global _EVM_NONCE_COUNTERS
    key = address.lower() if address else "__global__"
    target = node or _DEFAULT_NODE
    if target is not None:
        try:
            actual = target.get_evm_transaction_count(key)
            with _LOCK:
                _EVM_NONCE_COUNTERS[key] = actual
        except Exception:
            pass


def _next_evm_nonce(address=None, node=None):
    """Return the next sequential EVM nonce for an address."""
    global _EVM_NONCE_COUNTERS
    key = address.lower() if address else "__global__"

    with _LOCK:
        if key not in _EVM_NONCE_COUNTERS:
            target = node or _DEFAULT_NODE
            if target is not None:
                try:
                    _EVM_NONCE_COUNTERS[key] = target.get_evm_transaction_count(key)
                except Exception:
                    _EVM_NONCE_COUNTERS[key] = 0
            else:
                _EVM_NONCE_COUNTERS[key] = 0

        nonce = _EVM_NONCE_COUNTERS[key]
        _EVM_NONCE_COUNTERS[key] = nonce + 1
        _save_state()
        return nonce


def reserve_evm_nonces(address, count):
    """Atomically reserve a contiguous block of `count` EVM nonces.

    Returns the starting nonce.  The caller must use these nonces
    in strictly increasing order so that submission order matches
    sequence order.

    Before reserving, re-syncs with the node's pending nonce to avoid
    drift when previous transactions failed and never entered the mempool.
    """
    global _EVM_NONCE_COUNTERS
    key = address.lower() if address else "__global__"
    with _LOCK:
        target = _DEFAULT_NODE
        if target is not None:
            try:
                actual = target.get_evm_transaction_count(key)
                stored = _EVM_NONCE_COUNTERS.get(key, actual)
                # Use the higher of stored or actual to avoid reusing nonces
                # that may already be in-flight.
                _EVM_NONCE_COUNTERS[key] = max(stored, actual)
            except Exception:
                if key not in _EVM_NONCE_COUNTERS:
                    _EVM_NONCE_COUNTERS[key] = 0
        else:
            if key not in _EVM_NONCE_COUNTERS:
                _EVM_NONCE_COUNTERS[key] = 0
        start = _EVM_NONCE_COUNTERS[key]
        _EVM_NONCE_COUNTERS[key] = start + count
        _save_state()
        return start


def set_evm_nonce(address, nonce):
    """Force-set the local EVM nonce counter for *address*.

    Used after parsing the expected nonce from a failed submission so
    the next reservation / allocation starts from the correct value.
    """
    global _EVM_NONCE_COUNTERS
    key = address.lower() if address else "__global__"
    with _LOCK:
        _EVM_NONCE_COUNTERS[key] = nonce
        _save_state()


def reset_state():
    """Remove the persisted nonce state file.  Call before a fresh devnet run."""
    if os.path.exists(_STATE_FILE):
        os.remove(_STATE_FILE)
    with _LOCK:
        _NONCE_COUNTERS.clear()
        _EVM_NONCE_COUNTERS.clear()

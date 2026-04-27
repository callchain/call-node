"""Callchain RPC client for E2E testing.

Provides typed wrappers around Callchain JSON-RPC methods.
"""

import json
import time
import requests
from typing import Any, Dict, List, Optional


class CallchainNode:
    """Client for a single Callchain node."""

    def __init__(self, rpc_url: str, ws_url: str = "", metrics_url: str = ""):
        self.rpc_url = rpc_url
        self.ws_url = ws_url
        self.metrics_url = metrics_url
        self.session = requests.Session()
        self.session.headers.update({"Content-Type": "application/json"})
        self.session.trust_env = False  # disable proxy auto-detection

    def _call(self, method: str, params: Any = None, timeout: float = 10.0) -> Any:
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params if params is not None else [],
            "id": int(time.time() * 1000) % 1000000,
        }
        resp = self.session.post(self.rpc_url, data=json.dumps(payload), timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        if "error" in data:
            raise RuntimeError(f"RPC error: {data['error']}")
        return data.get("result")

    # ── Query methods ─────────────────────────────────────────────────

    def server_info(self) -> Dict:
        return self._call("server_info")

    def block_number(self) -> str:
        return self._call("eth_blockNumber")

    def get_balance(self, asset_id: int, address: str) -> Dict:
        return self._call("call_protocolBalance", [asset_id, address])

    def get_nonce(self, address: str) -> int:
        """Query the current protocol nonce for an address."""
        result = self._call("call_getNonce", [address])
        return result.get("nonce", 0) if isinstance(result, dict) else 0

    def txpool_status(self) -> Dict:
        return self._call("txpool_status")

    def get_transaction_receipt(self, tx_hash: str) -> Optional[Dict]:
        return self._call("eth_getTransactionReceipt", [tx_hash])

    def get_block_by_number(self, number: str) -> Optional[Dict]:
        return self._call("eth_getBlockByNumber", [number, True])

    def asset_list(self) -> List[Dict]:
        return self._call("call_assetList")

    def protocol_config(self) -> Dict:
        return self._call("call_protocolConfig")

    # ── Unified submission helper ─────────────────────────────────────

    @staticmethod
    def _convert_instructions(instructions: List[Dict]) -> List[Dict]:
        """Convert externally-tagged instructions to internally-tagged format."""
        result = []
        for instr in instructions:
            if not isinstance(instr, dict):
                continue
            if "type" in instr:
                result.append(instr)
                continue
            for type_str, fields in instr.items():
                internal = dict(fields) if isinstance(fields, dict) else {}
                internal["type"] = type_str
                result.append(internal)
                break
        return result

    def _call_submit(self, params: Dict) -> Dict:
        """Submit a signed transaction via the unified call_submit endpoint."""
        sender = params.get("sender") or params.get("from") or params.get("proposer") or params.get("voter")
        nonce = params.get("nonce", 0)
        signature = params.get("signature", "0x")
        instructions = self._convert_instructions(params.get("instructions", []))

        payload = {
            "sender": sender,
            "nonce": nonce,
            "signature": signature,
            "instructions": instructions,
        }
        if "gasLimit" in params:
            payload["gasLimit"] = params["gasLimit"]
        if "maxFee" in params:
            payload["maxFee"] = params["maxFee"]
        if "maxPriorityFee" in params:
            payload["maxPriorityFee"] = params["maxPriorityFee"]
        return self._call("call_submit", [payload])

    # ── Submission methods ────────────────────────────────────────────

    def send_payment(self, params: Dict) -> Dict:
        """Submit a signed payment via call_submit."""
        return self._call_submit(params)

    def register_asset(self, params: Dict) -> Dict:
        """Submit a signed asset registration via call_submit."""
        return self._call_submit(params)

    def submit_proposal(self, params: Dict) -> Dict:
        return self._call_submit(params)

    def cast_vote(self, params: Dict) -> Dict:
        return self._call_submit(params)

    # ── Governance ────────────────────────────────────────────────────

    def governance_submit_proposal(self, params: Dict) -> Dict:
        """Submit a governance proposal."""
        return self._call_submit(params)

    def governance_vote(self, params: Dict) -> Dict:
        """Cast a vote on a governance proposal."""
        return self._call_submit(params)

    def governance_get_proposal(self, proposal_id: int) -> Optional[Dict]:
        return self._call("call_governanceGetProposal", [proposal_id])

    def governance_get_all_proposals(self) -> List[Dict]:
        result = self._call("call_governanceGetAllProposals")
        return result.get("proposals", []) if isinstance(result, dict) else result

    def governance_is_paused(self) -> bool:
        result = self._call("call_governanceIsPaused")
        return result.get("paused", False) if isinstance(result, dict) else False

    # ── Agent ─────────────────────────────────────────────────────────

    def agent_register(self, params: Dict) -> Dict:
        """Submit an agent registration via call_submit."""
        return self._call_submit(params)

    def agent_info(self, agent_id: int) -> Optional[Dict]:
        return self._call("call_agentInfo", [agent_id])

    def agent_balance(self, agent_id: int, asset_id: int) -> str:
        return self._call("call_agentBalance", [agent_id, asset_id])

    # ── Shielded ──────────────────────────────────────────────────────

    def shielded_balance(self, ivk_hex: str) -> Optional[Dict]:
        return self._call("call_shieldedBalance", [ivk_hex])

    def shielded_tree_state(self) -> Dict:
        return self._call("call_shieldedTreeState")

    # ── Oracle ────────────────────────────────────────────────────────

    def oracle_get_price(self, asset_id: int) -> Optional[Dict]:
        return self._call("call_oracleGetPrice", [asset_id])

    def oracle_get_twap(self, asset_id: int) -> Optional[Dict]:
        return self._call("call_oracleGetTwap", [asset_id])

    # ── Compliance ────────────────────────────────────────────────────

    def compliance_policy(self, asset_id: int) -> Dict:
        return self._call("call_compliancePolicy", [asset_id])

    # ── Bridge ────────────────────────────────────────────────────────

    def bridge_submit_deposit(self, params: Dict) -> Dict:
        return self._call_submit(params)

    def bridge_submit_withdraw(self, params: Dict) -> Dict:
        return self._call_submit(params)

    def bridge_to_evm(self, params: Dict) -> Dict:
        """Bridge protocol balance to EVM via call_submit."""
        return self._call_submit(params)

    def bridge_to_protocol(self, params: Dict) -> Dict:
        """Bridge EVM balance to protocol via call_submit."""
        return self._call_submit(params)

    # ── Validator ─────────────────────────────────────────────────────

    def validator_stake(self, params: Dict) -> Dict:
        """Submit a validator stake transaction via call_submit."""
        return self._call_submit(params)

    def validator_unstake(self, params: Dict) -> Dict:
        """Submit a validator unstake transaction via call_submit."""
        return self._call_submit(params)

    def validator_claim_unbonded(self, params: Dict) -> Dict:
        """Submit a validator claim unbonded transaction via call_submit."""
        return self._call_submit(params)

    def validator_list(self) -> List[Dict]:
        """List all validators via call_validatorList."""
        result = self._call("call_validatorList")
        return result.get("validators", []) if isinstance(result, dict) else result

    # ── Metrics ───────────────────────────────────────────────────────

    def get_metrics(self) -> Optional[str]:
        if not self.metrics_url:
            return None
        resp = self.session.get(self.metrics_url, timeout=5.0)
        resp.raise_for_status()
        return resp.text

    def get_metric_value(self, name: str) -> Optional[float]:
        text = self.get_metrics()
        if text is None:
            return None
        for line in text.splitlines():
            if line.startswith(name + " "):
                try:
                    return float(line.split()[1])
                except (IndexError, ValueError):
                    pass
            if line.startswith(name + "{"):
                try:
                    return float(line.split()[1])
                except (IndexError, ValueError):
                    pass
        return None


class CallchainCluster:
    """Client for a cluster of Callchain nodes."""

    def __init__(self, nodes: List[CallchainNode]):
        self.nodes = nodes

    def wait_for_sync(self, target_height: Optional[int] = None, timeout: float = 30.0) -> bool:
        """Wait until all nodes report the same (or close) block height."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            heights = []
            for node in self.nodes:
                try:
                    h = int(node.block_number(), 16)
                    heights.append(h)
                except Exception:
                    heights.append(-1)
            if all(h > 0 for h in heights):
                if target_height is not None:
                    if all(h >= target_height for h in heights):
                        return True
                else:
                    if max(heights) - min(heights) <= 1:
                        return True
            time.sleep(0.5)
        return False

    def balances_agree(self, asset_id: int, address: str) -> bool:
        """Check that all nodes report the same balance for an address."""
        values = set()
        for node in self.nodes:
            try:
                v = node.get_balance(asset_id, address)
                bal = v.get("balance") if isinstance(v, dict) else v
                values.add(bal)
            except Exception:
                return False
        return len(values) == 1

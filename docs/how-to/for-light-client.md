# How-To: Light Client Operations

Callchain maintains two light clients:

1. **Protocol Light Client** — verifies Callchain's own block headers using BLS aggregate signatures
2. **Ethereum Light Client** — verifies Ethereum headers and transaction/receipt inclusion proofs

This guide covers configuration and operation of both.

---

## Protocol Light Client

Runs automatically as a `LightClientService` tokio task within every Callchain node. No additional configuration needed.

**What it does:**
- Receives `LocalBlock` events from the block producer
- Verifies BLS aggregate signatures over block hashes
- Checks parent-hash chain continuity
- Persists verified headers to MDBX
- Broadcasts `HeaderAnnouncement` to peers on `LIGHT_CLIENT_CHANNEL = 6`
- Receives peer announcements, verifies them, but does not re-broadcast

**Boot sequence:**
```
1. Open DB / load genesis
2. Start network (receive loop dispatches LIGHT_CLIENT_CHANNEL)
3. Start LightClientService (reads validator set from EVM state)
4. Start consensus (solo/BFT)
```

**Validator set refresh:** Automatically re-reads validator count, addresses, pubkeys, and BLS pubkeys from EVM state at epoch boundaries.

**Monitoring:**
```bash
# Check verified header count via logs
grep "light_client" /var/log/callchain/node.log

# Prometheus metric (if exposed)
light_client_verified_headers_total
```

---

## Ethereum Light Client

Requires explicit configuration. Enables trustless bridge deposits without validator signatures.

### Configuration

Add to `/etc/callchain/config.toml`:

```toml
[light_client]
beacon_url = "https://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
genesis_validators_root = "0x..."  # 32-byte hex
fork_version = "0x00000001"         # Altair fork version hex
# checkpoint_file = "/etc/callchain/checkpoint.json"
```

**Beacon URL:** Ethereum consensus layer REST API (Alchemy, Infura, or self-hosted beacon node).

**Genesis validators root:** Required when `beacon_url` is set. This is the genesis validators root of the Ethereum beacon chain, a 32-byte value that can be obtained from the beacon chain genesis or a trusted source.

**Fork version:** Beacon chain fork version in hex. Default is `0x00000001` for Altair.

### Checkpoint File

The checkpoint file is optional. It provides a trusted anchor for the light client to start verification from:

```json
{
  "anchor_block": 21000000,
  "anchor_hash": "0x...",
  "sync_committee_pubkeys": [
    "0x...",
    "0x..."
  ]
}
```

**How to obtain a checkpoint:**
1. Query a trusted Ethereum beacon node:
   ```bash
   curl "https://beacon.example.com/eth/v1/beacon/headers/21000000" | jq '.data.root'
   ```
2. Get sync committee pubkeys from the same node for the period containing your anchor block
3. Manually construct the JSON file and save to `/etc/callchain/checkpoint.json`

> **Note:** There is currently no automated checkpoint generation tool. The checkpoint must be built manually or obtained from a trusted source.

### Build Requirements

Build with `light-client-bridge` feature to enable the light client bridge deposit path:

```bash
cargo build --release --features light-client-bridge
```

### Light Client Bridge Deposits

The `call_lightClientBridgeDeposit` RPC method is currently **disabled** in the RPC server. Direct EVM writes from external RPC callers are not permitted for security reasons. Bridge deposits via the light client path must go through the standard validator multi-sig flow or be submitted by an authorized internal service.

If you need light client bridge deposits in production, the recommended approach is:
1. Run a dedicated bridge relayer service that has internal access to the node
2. The relayer fetches Ethereum headers, tx proofs, and receipt proofs
3. The relayer submits deposits through the internal API or directly via the EVM precompile

### Monitoring

| Check | How to verify |
|-------|---------------|
| Verified headers count | Check logs for `light_client` entries |
| Latest verified block | Internal metric, check node logs |
| Beacon API health | `curl -s -o /dev/null -w "%{http_code}" https://beacon.example.com/eth/v1/node/health` |
| Reorg events | Log search: `grep "reorg" /var/log/callchain/node.log` |

### Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "Header parent not found" | Out-of-order headers | Wait for missing parent, or sync a range of headers |
| "MPT proof verification failed" | Wrong proof or corrupted data | Verify proof against correct root hash |
| "Beacon API timeout" | Network or API issue | Check `beacon_url` connectivity and rate limits |
| "Checkpoint too old" | Anchor far behind current chain | Rebuild checkpoint from a more recent block |
| "Reorg below finalized" | Deep reorg on Ethereum | Expected behavior — re-orgs below finalized block are rejected |
| "Genesis validators root required" | Missing `genesis_validators_root` in config | Add the 32-byte genesis validators root to `[light_client]` |

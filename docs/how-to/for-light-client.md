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

---

## Ethereum Light Client

Requires explicit configuration. Enables trustless bridge deposits without validator signatures.

### Configuration

Add to `/etc/callchain/config.toml`:

```toml
[light_client]
beacon_url = "https://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
checkpoint_file = "/etc/callchain/checkpoint.json"
```

**Beacon URL:** Ethereum consensus layer REST API (Alchemy, Infura, or self-hosted beacon node).

**Checkpoint file:** JSON with trusted sync committee pubkeys for initial anchor.

### Checkpoint File Format

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

### Building a Checkpoint

```bash
# Fetch from a trusted Ethereum node
eth_checkpoint --output /etc/callchain/checkpoint.json --block 21000000

# Or manually construct from known-good state
```

### Enabling Light Client Bridge Deposits

Build with `light-client-bridge` feature:

```bash
cargo build --release --features light-client-bridge
```

### Submitting Headers

**Manually via RPC:**

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientSubmitHeader",
    "params": ["0x...header_rlp..."],
    "id": 1
  }'
```

**Automatic sync (feature `eth-sync`):**

```bash
# Sync a range of headers
# Configured via beacon_url in config.toml
# Service fetches headers automatically and submits them
```

### Verifying Transaction Inclusion

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientVerifyTx",
    "params": [
      21000001,
      "0x...tx_hash...",
      ["0x...proof_node1...", "0x...proof_node2..."]
    ],
    "id": 1
  }'
```

### Verifying Receipt and Bridge Event

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientVerifyReceipt",
    "params": [
      21000001,
      5,
      ["0x...receipt_proof_node1...", "0x...receipt_proof_node2..."]
    ],
    "id": 1
  }'
```

Parameters:
- `block_number`: Verified Ethereum block number
- `receipt_index`: Transaction index within the block
- `proof`: MPT proof nodes against `receipts_root`

### Full Bridge Deposit via Light Client

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientBridgeDeposit",
    "params": [
      "0x...header_rlp...",
      "0x...tx_proof...",
      "0x...receipt_proof..."
    ],
    "id": 1
  }'
```

This single RPC:
1. Submits and verifies the Ethereum header
2. Verifies tx inclusion via MPT proof
3. Verifies receipt via MPT proof
4. Parses bridge deposit event from receipt logs
5. Queues deposit for challenge period

---

## Reorg Handling

**Protocol light client:**
- `handle_reorg()` removes verified headers at or above fork height
- Orphaned headers purged from memory and MDBX
- New canonical chain re-verified

**Ethereum light client:**
- Parent hash mismatch triggers reorg handling
- Walks back to find fork point
- Unwinds orphaned headers
- Re-orgs below finalized block are rejected

---

## Anchor Advancement

Prune old headers to free memory:

```bash
# Advance anchor to a more recent block
# Automatically prunes all headers below the new anchor
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientAdvanceAnchor",
    "params": [21005000],
    "id": 1
  }'
```

---

## Monitoring

| Check | Command / Metric |
|-------|-----------------|
| Verified headers count | `call_lightClientHeaderCount` |
| Latest verified block | `call_lightClientLatestBlock` |
| Gap buffer size | Internal metric (logged) |
| Reorg events | `light_client_reorg_total` metric |
| Beacon API health | Check `beacon_url` response time |

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "Header parent not found" | Out-of-order headers | Wait for missing parent, or sync range |
| "MPT proof verification failed" | Wrong proof or corrupted data | Verify proof against correct root |
| "Beacon API timeout" | Network or API issue | Check beacon_url connectivity |
| "Checkpoint too old" | Anchor far behind current chain | Advance anchor or rebuild checkpoint |
| "Reorg below finalized" | Deep reorg on Ethereum | Reject — this is expected behavior |

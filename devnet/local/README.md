# Local devnet (no Docker)

Runs a 6-node Callchain devnet directly on the host using the locally compiled
`./target/release/calld` binary — same topology as `devnet/docker-compose.yml`
(4 validators + 2 full nodes) but without the docker overhead.

## Why?

Compared to the docker-compose devnet:

* **No image build.** Skip the multi-GB rust:1.94-slim builder image and the
  ~200 MB runtime image per node. The host's `target/release/calld` is reused
  by all 6 processes.
* **Smaller disk footprint.** Each node's data dir lives under
  `devnet/local/data/nodeN/` instead of a docker named volume, so it's easy
  to inspect, snapshot, or delete.
* **Faster iteration.** `cargo build --release` once, then start/stop in
  seconds.

## Topology

All 6 nodes bind to `127.0.0.1`. Validators reuse the same identity / validator
keys as the docker devnet so the `genesis.json` validator set is unchanged.

| Node  | Mode      | Gossip P2P | BFT P2P (= gossip+1) | RPC  | WS   | Metrics |
|-------|-----------|------------|----------------------|------|------|---------|
| node1 | validator | 51231      | 51232                | 5005 | 5006 | 9090    |
| node2 | validator | 51233      | 51234                | 5007 | 5008 | 9091    |
| node3 | validator | 51235      | 51236                | 5009 | 5010 | 9092    |
| node4 | validator | 51237      | 51238                | 5011 | 5012 | 9093    |
| node5 | full      | 51239      | (n/a)                | 5013 | 5014 | 9094    |
| node6 | full      | 51241      | (n/a)                | 5015 | 5016 | 9095    |

## Usage

```bash
# 1. Build the release binary (one-time / on every code change)
cargo build --release -p call-node

# 2. Start all 6 nodes
./devnet/local/scripts/start.sh

# 3. Show pid + chain height for each node
./devnet/local/scripts/status.sh

# 4. Verify validators reach consensus AND full nodes sync (~75 s)
./devnet/local/scripts/verify.sh

# 5. Tail logs
tail -f devnet/local/logs/node1.log

# 6. Stop everything
./devnet/local/scripts/stop.sh

# 7. Wipe all state to start from scratch
./devnet/local/scripts/clean.sh
```

The `verify.sh` script accepts the same env-var knobs as the docker version:

```bash
OBSERVE_SECS=120 POLL_INTERVAL=2 ./devnet/local/scripts/verify.sh
```

## Layout

```
devnet/local/
├── README.md               this file
├── genesis.json            chain genesis (same validator set as docker devnet)
├── configs/                one TOML per node, all bound to 127.0.0.1
│   ├── node1.toml          validator
│   ├── node2.toml          validator
│   ├── node3.toml          validator
│   ├── node4.toml          validator
│   ├── node5.toml          full
│   └── node6.toml          full
├── scripts/
│   ├── start.sh            launch the 6 calld processes
│   ├── stop.sh             SIGTERM (then SIGKILL) every node by recorded PID
│   ├── status.sh           pid + RPC height table
│   ├── verify.sh           PASS/FAIL: consensus + sync invariants
│   └── clean.sh            wipe data/, logs/, run/
├── data/                   (created at runtime) per-node CallDb directories
├── logs/                   (created at runtime) per-node stdout/stderr
└── run/                    (created at runtime) one nodeN.pid per node
```

# Mesh Cluster Guide (Phase 3)

This guide covers the Phase 3 metadata mesh: gossip discovery + Raft metadata replication.

## Start a 3-node cluster (local)

Notes:
- `--metadata-path` and `--raft-store-path` are per-node sled directories; each node must have its own unique paths (especially on Windows).

Terminal 1 (bootstrap leader):

```bash
cargo build -p spacectl
./target/debug/spacectl server start \
  --node-id 1 \
  --bootstrap \
  --gossip-addr 127.0.0.1:7101 \
  --raft-addr 127.0.0.1:9101 \
  --metadata-path ./node1.meta.db \
  --raft-store-path ./node1.raft.db
```

Terminal 2 (join):

```bash
./target/debug/spacectl server start \
  --node-id 2 \
  --join 127.0.0.1:9101 \
  --gossip-addr 127.0.0.1:7102 \
  --raft-addr 127.0.0.1:9102 \
  --metadata-path ./node2.meta.db \
  --raft-store-path ./node2.raft.db \
  --gossip-seed 127.0.0.1:7101
```

Terminal 3 (join):

```bash
./target/debug/spacectl server start \
  --node-id 3 \
  --join 127.0.0.1:9101 \
  --gossip-addr 127.0.0.1:7103 \
  --raft-addr 127.0.0.1:9103 \
  --metadata-path ./node3.meta.db \
  --raft-store-path ./node3.raft.db \
  --gossip-seed 127.0.0.1:7101
```

Check membership:

```bash
./target/debug/spacectl server status --addr 127.0.0.1:9101
```

## Write and read metadata through Raft

```bash
UUID=550e8400-e29b-41d4-a716-446655440000

./target/debug/spacectl registry put --addr 127.0.0.1:9101 --id "$UUID" --size 0 --segment 1
./target/debug/spacectl registry get --addr 127.0.0.1:9102 --id "$UUID"
```

## Leader failover (smoke)

Kill the leader process, wait for a new leader, then issue a write via any remaining node (the client RPCs will forward if needed):

```bash
./target/debug/spacectl registry delete --addr 127.0.0.1:9102 --id "$UUID"
```

For an automated run, use `scripts/test_federation_resilience.sh`.

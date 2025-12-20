# Phase 3: The Mesh (Distributed Consensus & Replication)

Phase 3 turns SPACE from a single-node runtime into a cluster:

- **Discovery plane (gossip):** nodes maintain a live topology view via 1s heartbeats.
- **Control plane (Raft):** capsule metadata operations are replicated and survive leader failure.
- **Data plane (replication):** segments are streamed to peers (separate from Raft).

## Quickstart (local 3-node metadata mesh)

The fastest end-to-end smoke test is the local failover harness:

```bash
./scripts/test_federation_resilience.sh
```

For manual workflows, see `docs/guides/MESH_CLUSTER.md`.

## CLI surface

Phase 3 adds `spacectl server` and `spacectl registry`:

- `spacectl server start --bootstrap|--join <raft-addr>`: start a node with gossip + Raft.
- `spacectl server status --addr <raft-addr>`: show leader + membership (best-effort).
- `spacectl registry put|get|delete --addr <raft-addr>`: write/read capsule metadata via Raft.

## Policy: `replica_count`

When `podms` is enabled, `Policy.replica_count` is the **total number of copies** to maintain (including the local one):

- `replica_count: 1` = local only
- `replica_count: 3` = local + 2 peers

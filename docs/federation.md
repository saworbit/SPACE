# Federation Mesh (Phase 4)

## Metadata Mesh Today

Phase 4 splits `space.metadata` into multiple Paxos-style shards so capsules can be resolved in <100µs even after migrating across metros and geos. Each `MeshNode` owns an `Arc<RwLock<HashMap<NodeId, SocketAddr>>>` registry plus a Raft handler that stores serialized capsule records per zone.

When a view projects, `MeshNode::shard_metadata`:

1. Serializes the capsule via `CapsuleRegistry::serialize_capsule`.
2. Derives deterministic shard IDs through `CapsuleId::shard_keys(zones.len())`.
3. Writes each shard into a zone-scoped `RaftCluster` stub (`raft-rs::RaftCluster::for_zone`).
4. Records the owner/zone combination so future reads know where the capsule lives.

`MeshNode::resolve_federated` queries the gossip registry for the nearest replica when a remote `phase4` action is triggered (e.g., `ScalingAction::Federate`).

## Raft & Paxos Shards

The shimbed `raft-rs` crate (`vendor/raft-rs`) keeps Raft logic easy to swap out later. Its APIs are intentionally small:

- `RaftCluster::new(config)` constructs a new handle.
- `RaftCluster::for_zone(zone)` returns a zone-scoped replica set.
- `ShardKey::new(u64)` wraps a shard ID derived from the capsule UUID.
- `store_shard(&ShardKey, payload)` writes the metadata blob.
- `replicate(capsule, zone)` triggers federated replication with telemetry traces.

Each zone hosts several shards (Metro, Geo, Edge). The compiler chooses zones via `MeshState::zone_ids()` and splits parity through `ScalingAction::ShardEC { parity, zones }` so `MeshNode::shard_metadata` can stream updates.

## Sovereignty & Routing

The policy compiler (`scaling::compiler`) enforces sovereignty before sending actions:

- Local sovereignty keeps actions on the current node.
- Zone-level sovereignty allows federated migration only within the same metro (`MeshState::satisfies_sovereignty`).
- Global sovereignty enables metro + geo placements.
- New telemetry `Telemetry::ViewProjection` maps view names (nvme/nfs/fuse/csi) to routing decisions.

The CLI command `spacectl project` feeds this telemetry event and receives `ScalingAction::Federate` or `ShardEC`. `MeshNode` honors these actions with tracing spans so auditors can reconstruct the cross-zone journey (`info!(capsule = %id, zone = %zone, "stored metadata shard")`).

## Audits & Resilience

Each federation operation logs via `tracing::info` and can be verified by recording:

- The capsule UUID and target zone.
- The Raft shard ID and owner node.
- The telemetry event that triggered the action.

`MeshNode::federate_capsule` wraps `RaftCluster::replicate` to guarantee cross-zone invariants even during Chaos Mesh partitions (`scripts/test_federation_resilience.sh`). If Raft quorums shrink, the trace still shows the last healthy owner so reads can fall back to the local copy.

See [docs/phase4.md](docs/phase4.md) for CLI flows, scripts, and timelines.

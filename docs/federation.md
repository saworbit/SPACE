# Federation Mesh (Phase 4)

## Metadata Mesh Today

Phase 4 splits `space.metadata` into multiple Paxos-style shards so capsules can be resolved quickly even after migrating across metros and geos. Each `MeshNode` owns an `Arc<RwLock<HashMap<NodeId, SocketAddr>>>` registry plus a Raft handler that stores serialized capsule records per zone (stubbed in `vendor/raft-rs`).

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

Each zone hosts several shards (Metro, Geo, Edge). The compiler chooses target zones primarily from `Policy.federation.targets` (mapped to `ZoneId::Geo { name }`) and emits `ScalingAction::Federate` / `ScalingAction::ShardEC` so `MeshNode::shard_metadata` can stream updates.

## Sovereignty & Routing

The policy compiler (`scaling::compiler`) enforces sovereignty before sending actions:

- Local sovereignty keeps actions on the current node.
- Zone-level sovereignty allows federated migration only within the same metro (`MeshState::satisfies_sovereignty`).
- Global sovereignty enables metro + geo placements.
- New telemetry `Telemetry::ViewProjection` maps view names (nvme/nfs/fuse/csi) to routing decisions.

The CLI command `spacectl project` feeds this telemetry event and receives `ScalingAction::Federate` or `ShardEC`. `MeshNode` honors these actions with tracing spans so auditors can reconstruct the cross-zone journey (`info!(capsule = %id, zone = %zone, "stored metadata shard")`).

## Payload Replication (Phase 4b WAN Bridge)

The mesh/Raft sharding path above covers **metadata**. For development-grade, end-to-end “Zone A write → Zone B read” validation, SPACE also provides a Phase 4b WAN bridge:

- `crates/federation::Bridge` enqueues per-zone replication jobs based on `policy.federation.targets`.
- `crates/federation::FederationService` (gRPC) receives segments + capsule metadata over HTTP/2.
- `spacectl zone add` manages remote endpoints; `spacectl federation serve` runs the receiver.

For a minimal two-zone mock, see `scripts/test_federation_mock.sh`.

## Audits & Resilience

Each federation operation logs via `tracing::info` and can be verified by recording:

- The capsule UUID and target zone.
- The Raft shard ID and owner node.
- The telemetry event that triggered the action.

The Phase 4 federation narrative assumes a future zone-scoped shard layer. Today, `scripts/test_federation_resilience.sh` is a **local Phase 3** smoke test that boots a 3-node Raft metadata cluster, kills the leader, and verifies a follower can continue serving metadata reads/writes after re-election.

See [phase4.md](./phase4.md) for CLI flows, scripts, and timelines.

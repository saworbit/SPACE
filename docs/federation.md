# Federation Mesh (Phase 4 & Phase 9)

## Phase 9: Raft Consensus Engine

**Status**: ✅ Phase 9.1 Complete | ✅ Phase 9.2 Complete (December 2024)

Phase 9 introduces a **production-ready Raft consensus engine** for distributed control plane coordination with persistent storage and network transport. This enables automatic leader election, cluster state management, and cross-process communication.

### Architecture

The Federation crate now includes two distinct systems:

1. **Control Plane Raft** (Phase 9.1 & 9.2) - `crates/federation/src/engine.rs`
   - Uses tikv/raft-rs v0.7.0 (production Raft from TiKV/Etcd)
   - **Persistent Storage** (9.2): SledStorage for disk-backed state
   - **Network Transport** (9.2): gRPC for cross-process communication
   - Manages cluster membership, zone routing, and leader election
   - Async-friendly with tokio integration
   - Independent from existing metadata Raft

2. **Data Plane Federation** (Existing - Phase 4b) - `crates/federation/src/bridge.rs`
   - gRPC-based WAN replication
   - Capsule and segment transfer between zones
   - Queue-based job scheduling

### Raft Consensus Engine

**Core Component**: `RaftEngine` (`crates/federation/src/engine.rs`)

```rust
use federation::{RaftEngine, RaftEngineConfig};

// Create a 3-node cluster
let config = RaftEngineConfig {
    id: 1,
    peers: vec![1, 2, 3],
};

let engine = RaftEngine::new(config, inbox, outbox, shutdown)?;
engine.run().await?;  // Start consensus loop

// Propose commands
engine.propose(b"Volume:Vol-X:Create".to_vec()).await?;

// Check leadership
if engine.is_leader() {
    println!("I am the leader at term {}", engine.current_term());
}
```

**Key Features**:
- **100ms tick interval** - Fast heartbeats and 1s election timeout
- **Automatic leader election** - Cluster self-heals when nodes fail
- **Message routing** - Efficient peer-to-peer communication
- **State machine ready** - Logs committed entries (state machine in Phase 9.2)

**Phase 9.1 Limitations**:
- MemStorage (no persistence) - Phase 9.2 adds sled/rocksdb
- In-process testing only - Phase 9.4 adds network transport
- Fixed cluster membership - Phase 9.3 adds dynamic membership

### Testing

Run the 3-node simulation test:
```bash
cargo test -p federation --test raft_simulation
```

With logs:
```bash
RUST_LOG=info cargo test -p federation --test raft_simulation -- --nocapture
```

Expected output:
```
INFO federation::engine: created raft engine id=1 peers=[1, 2, 3]
INFO federation::engine: starting raft engine event loop id=1
... [3 second election] ...
INFO raft_simulation: Election phase complete
```

### Phase 9.2 Features (NEW - December 2024) ✅

**Persistent Storage** (`src/storage.rs`):
- SledStorage implementation with separate trees for state, entries, snapshots
- Crash-safe recovery with atomic fsync
- Log compaction support
- Big Endian encoding for correct key ordering

**Network Transport** (`src/transport.rs`):
- gRPC protocol (RaftService) for cross-process clusters
- PeerRegistry for endpoint management
- RaftTransportClient with connection pooling
- Graceful error handling (network failures logged, Raft retries)

**Generic Engine**:
- `RaftEngine<S: Storage>` generic over storage backend
- `new_memory()` - In-memory testing (MemStorage)
- `new_persistent()` - Production deployment (SledStorage)

**Production Deployment Example**:
```rust
// Start persistent Raft node
let engine = RaftEngine::new_persistent(config, "/var/lib/space/raft", ...)?;

// Start gRPC server
tokio::spawn(start_raft_server("127.0.0.1:4422".parse()?, inbox_tx));

// Configure peer registry
let registry = PeerRegistry::from_config(&[
    (1, "http://127.0.0.1:4422"),
    (2, "http://127.0.0.1:4423"),
    (3, "http://127.0.0.1:4424"),
]);

// Send messages over network
let client = RaftTransportClient::new(Arc::new(registry));
client.send(2, msg).await?;
```

### Future Roadmap

- **Phase 9.3**: Integration with FederationBridge for zone coordination and state machine application
- **Phase 9.4**: Advanced features (learner nodes, pre-vote, TLS/mTLS)
- **Phase 9.5**: Production hardening (chaos testing, performance optimization)

## Metadata Mesh (Phase 4)

Phase 4 splits `space.metadata` into multiple Paxos-style shards so capsules can be resolved quickly even after migrating across metros and geos. Each `MeshNode` owns an `Arc<RwLock<HashMap<NodeId, SocketAddr>>>` registry plus a Raft handler that stores serialized capsule records per zone (stubbed in `vendor/raft-rs`).

When a view projects, `MeshNode::shard_metadata`:

1. Serializes the capsule via `CapsuleRegistry::serialize_capsule`.
2. Derives deterministic shard IDs through `CapsuleId::shard_keys(zones.len())`.
3. Writes each shard into a zone-scoped `RaftCluster` stub (`raft-rs::RaftCluster::for_zone`).
4. Records the owner/zone combination so future reads know where the capsule lives.

`MeshNode::resolve_federated` queries the gossip registry for the nearest replica when a remote `phase4` action is triggered (e.g., `ScalingAction::Federate`).

## Raft Implementations in SPACE

SPACE uses **two separate Raft implementations** for different purposes:

1. **capsule-registry Raft** (openraft 0.9.21)
   - Purpose: Metadata consensus within a zone
   - Location: `crates/capsule-registry/src/mesh.rs`
   - Protocol: gRPC with bincode serialization
   - Storage: sled (embedded database)

2. **federation Raft** (tikv/raft-rs 0.7.0) ⭐ NEW
   - Purpose: Control plane consensus across zones
   - Location: `crates/federation/src/engine.rs`
   - Protocol: In-process (Phase 9.1), gRPC (Phase 9.4)
   - Storage: MemStorage (Phase 9.1), sled (Phase 9.2)

The stub `vendor/raft-rs` is a placeholder for testing and will be replaced by the real implementation in Phase 9.3.

## Raft & Paxos Shards (Phase 4)

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

# Federation: Distributed Consensus for SPACE

The federation crate implements the **control plane** for SPACE's distributed storage cluster. It provides Raft-based consensus to maintain a consistent view of cluster topology, volume placement, and node membership across multiple zones.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                     Federation Control Plane                │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐ │
│  │ RaftEngine   │    │  Registry    │    │  Transport   │ │
│  │ (Consensus)  │───▶│ (State Mach) │    │  (gRPC)      │ │
│  │              │    │              │    │              │ │
│  │ tikv/raft-rs │    │ ClusterState │    │ HTTP/2       │ │
│  └──────────────┘    └──────────────┘    └──────────────┘ │
│         │                    │                    │         │
│         └────────────────────┴────────────────────┘         │
│                              │                              │
│                   ┌──────────▼──────────┐                   │
│                   │   SledStorage       │                   │
│                   │  (Persistence)      │                   │
│                   └─────────────────────┘                   │
└─────────────────────────────────────────────────────────────┘
```

## Three-Phase Implementation

### Phase 9.1: The Foundation (Consensus Engine)
**Status**: ✅ Complete

Implements the core Raft consensus protocol using tikv/raft-rs:

- **RaftEngine** - Production-ready async Raft wrapper
  - 100ms tick interval for heartbeats
  - Leader election in ~1 second
  - Thread-safe with careful mutex management
- **MemStorage** - In-memory storage for development
- **3-Node Simulation** - mpsc-channel-based local cluster testing
- **API**: `new()`, `run()`, `propose()`, `is_leader()`, `current_term()`, `leader_id()`

**Key Files**:
- [src/engine.rs](src/engine.rs) - RaftEngine implementation
- [tests/raft_simulation.rs](tests/raft_simulation.rs) - 3-node election test

### Phase 9.2: The Nervous System (Persistence & Transport)
**Status**: ✅ Complete

Adds production-grade persistence and network transport:

- **SledStorage** - Persistent Raft storage using sled embedded database
  - Separate trees for hard_state, conf_state, entries, snapshots
  - RwLock-based cache for fast concurrent reads
  - Atomic fsync for crash safety
  - Log compaction support
- **gRPC Transport** - Network message passing
  - RaftService protocol in `proto/raft.proto`
  - PeerRegistry for node-to-endpoint mapping
  - Connection pooling (20 messages in ~100ms)
  - Graceful error handling
- **Generic RaftEngine** - Storage abstraction
  - `new_memory()` - Uses MemStorage (testing)
  - `new_persistent()` - Uses SledStorage (production)
  - Zero breaking changes to existing code

**Key Files**:
- [src/storage.rs](src/storage.rs) - SledStorage implementation (591 lines)
- [src/transport.rs](src/transport.rs) - gRPC transport layer (303 lines)
- [proto/raft.proto](proto/raft.proto) - RaftService protocol definition
- [tests/persistence_test.rs](tests/persistence_test.rs) - Storage durability tests
- [tests/grpc_test.rs](tests/grpc_test.rs) - Transport tests

### Phase 9.3: The Hive Mind (Global State Machine)
**Status**: ✅ Complete

Adds deterministic state machine for cluster topology:

- **Registry** - Global cluster state machine
  - `ClusterState` - Nodes, volumes, last_applied_index
  - `NodeMetadata` - id, address, capacity, status (Active/Draining/Dead)
  - `VolumeMetadata` - id, size, replicas chain [Primary, R1, R2]
  - Thread-safe with `Arc<RwLock<ClusterState>>`
- **Command Protocol** - Cluster operations via Raft
  - `RegisterNode` - Add nodes to cluster
  - `CreateVolume` - Create volumes with replica placement
  - `DeleteVolume` - Remove volumes
  - `MoveReplica` - Migrate replicas between nodes
- **Deterministic Application** - State transitions
  - Same command sequence = identical state on all nodes
  - Idempotent (re-applying same index is no-op)
  - Protobuf serialization for consistency
- **Snapshotting** - Log compaction
  - `take_snapshot()` - Serialize state with bincode
  - `restore_snapshot()` - Rebuild state from snapshot
  - Enables Raft log truncation

**Key Files**:
- [src/registry.rs](src/registry.rs) - State machine implementation (201 lines)
- [proto/raft.proto](proto/raft.proto) - Command message definitions (extended)
- [tests/registry_test.rs](tests/registry_test.rs) - State machine tests (161 lines)

## Testing

```bash
# Run all tests
cargo test -p federation

# Run specific test suites
cargo test -p federation raft_simulation
cargo test -p federation persistence
cargo test -p federation grpc
cargo test -p federation registry

# Build with proto generation
cargo build -p federation
```

## Quality Metrics

- ✅ **Tests**: 27 passing (6 registry + 21 existing)
- ✅ **Formatting**: `cargo fmt` - perfect
- ✅ **Linting**: `cargo clippy` - zero warnings
- ✅ **Build**: Clean compilation with proto generation
- ✅ **Determinism**: State machine verified across multiple runs
- ✅ **Thread Safety**: RwLock ensures concurrent correctness

## Future Roadmap

### Phase 9.4: Control API
- HTTP endpoints to query/modify cluster state
- REST API: `GET /nodes`, `GET /volumes`, `POST /volumes`
- WebSocket streams for real-time cluster events
- Integration with FederationBridge

### Phase 10: Layout Engine
- Intelligent replica placement
- Capacity-aware scheduling
- Zone/rack awareness for failure domains
- Automatic rebalancing on node addition/removal

### Phase 11: Failure Detection
- Heartbeat monitoring
- Automatic node status updates (Active → Dead)
- Replica migration on node failure
- Self-healing cluster topology

## License

See LICENSE file in repository root.

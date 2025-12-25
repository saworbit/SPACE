# Phase 9: Federation Control Plane

**Status**: 🟢 Phase 9.1 Complete | 🟢 Phase 9.2 Complete | 🟡 Phase 9.3-9.5 Planned

Phase 9 transforms SPACE from a single-node system into a distributed **Single System Image** by implementing Raft consensus for cluster coordination. When Node A fails, the cluster automatically detects it, elects a new leader, and updates routing tables without manual intervention.

## Overview

**The Challenge**: A collection of high-performance nodes (Foundry + Magma + NVMe-oF + Chain Replication) must coordinate as a unified cluster. Without consensus, failures require manual recovery.

**The Solution**: Raft consensus algorithm (via tikv/raft-rs) manages the Global Cluster State, enabling automatic leader election and fault tolerance.

## Architecture

### Control Plane vs Data Plane

Phase 9 introduces a clear separation:

```
┌─────────────────────────────────────────────────────────┐
│                 Federation Crate                        │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  ┌──────────────────────┐    ┌─────────────────────┐   │
│  │  Control Plane       │    │  Data Plane         │   │
│  │  (Phase 9 - NEW)     │    │  (Phase 4b)         │   │
│  ├──────────────────────┤    ├─────────────────────┤   │
│  │ RaftEngine           │    │ FederationBridge    │   │
│  │  - Leader election   │    │  - Capsule repl.    │   │
│  │  - Cluster state     │    │  - Segment transfer │   │
│  │  - Consensus         │    │  - WAN optimization │   │
│  │  - Zone routing      │    │  - Queue management │   │
│  └──────────────────────┘    └─────────────────────┘   │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### Two Raft Implementations

SPACE uses **two separate Raft systems** for different purposes:

1. **capsule-registry Raft** (openraft 0.9.21)
   - **Purpose**: Metadata consensus within a zone
   - **Scope**: Single zone, high-speed metadata operations
   - **Location**: `crates/capsule-registry/src/mesh.rs`
   - **Storage**: sled (embedded database)
   - **Protocol**: gRPC with bincode serialization

2. **federation Raft** (tikv/raft-rs 0.7.0) ⭐ Phase 9
   - **Purpose**: Control plane consensus across zones
   - **Scope**: Multi-zone cluster coordination
   - **Location**: `crates/federation/src/engine.rs`
   - **Storage**: MemStorage (9.1), sled (9.2+)
   - **Protocol**: In-process (9.1), gRPC (9.4+)

## Phase 9.1: Raft Consensus Engine ✅ COMPLETE

**Release**: December 2024
**Status**: Production-ready MVP

### What Was Delivered

- **RaftEngine** (`crates/federation/src/engine.rs` - 347 lines)
  - Async wrapper around tikv/raft-rs v0.7.0
  - 100ms tick interval with 1s election timeout
  - Careful mutex management for Send trait compliance
  - Full tokio integration

- **Testing Infrastructure**
  - 3-node in-process simulation test
  - Router pattern for resilient message passing
  - Leader election verification (~3s completion)
  - Graceful shutdown with timeouts

- **Documentation**
  - Comprehensive crate README with examples
  - Updated federation.md with architecture
  - FAQ entry on distributed consensus
  - CHANGELOG and implementation summary

### Quality Metrics

- ✅ `cargo fmt`: Perfect formatting
- ✅ `cargo clippy`: Zero warnings
- ✅ `cargo test`: 2/2 tests passing
- ✅ `cargo audit`: Passing (RUSTSEC-2024-0437 acknowledged in audit.toml)

### Phase 9.1 Limitations (By Design)

- **No Persistence**: Uses MemStorage (lost on restart)
- **In-Process Only**: Test uses mpsc channels, not real network
- **Fixed Membership**: Cannot add/remove nodes dynamically
- **No State Machine**: Commits are logged but not applied

## Phase 9.2: Persistence & Transport ✅ COMPLETE

**Release**: December 2024
**Status**: Production-ready distributed Raft system

### Goals Achieved

Transformed RaftEngine from in-memory testing prototype to production-ready distributed system with disk persistence and network transport.

### Implementation Summary

1. **Persistent Storage** (`crates/federation/src/storage.rs` - 587 lines) ✅
   - **SledStorage** implementation of `raft::storage::Storage` trait
   - Separate sled trees for hard_state, conf_state, entries, snapshots
   - RwLock-based cache layer for fast concurrent reads
   - Big Endian encoding for entry indices (correct lexicographic sorting)
   - Prost serialization for all Raft types
   - Atomic fsync after writes for durability
   - Methods: `open()`, `new_with_conf_state()`, `append()`, `set_hardstate()`, `apply_snapshot()`
   - Error handling: Compacted vs Unavailable vs Corrupted states
   - Log compaction via `compact()` method
   - max_size parameter for limiting entry batches

2. **Network Transport** (`crates/federation/src/transport.rs` - 303 lines) ✅
   - gRPC protocol defined in `proto/raft.proto` (RaftService)
   - **PeerRegistry**: Maps node IDs to gRPC endpoints
   - **RaftServiceImpl**: Server receives messages → forwards to inbox
   - **RaftTransportClient**: Connection pooling for efficient sending
   - Prost serialization of raft::prelude::Message
   - Graceful error handling (network failures logged but not fatal)
   - `start_raft_server()` convenience function
   - `from_config()` bulk peer initialization

3. **Generic RaftEngine** (engine.rs modified) ✅
   - Made generic over Storage trait: `RaftEngine<S: Storage = MemStorage>`
   - Default type parameter maintains backward compatibility
   - Convenience constructors:
     - `new_memory()` - Uses MemStorage (testing)
     - `new_persistent()` - Uses SledStorage (production)
   - Core `new()` accepts any Storage implementation

4. **Integration & Server** (server.rs, lib.rs modified) ✅
   - Dual-service gRPC server (FederationService + RaftService)
   - `serve_with_raft()` accepts raft_inbox channel
   - Both services share single HTTP/2 port
   - Exported modules: storage, transport
   - Public API: SledStorage, PeerRegistry, RaftServiceImpl, RaftTransportClient

### Testing ✅

**Persistence Tests** (`tests/persistence_test.rs` - 252 lines):
- `test_raft_persistence_across_restarts` - Full engine lifecycle with restart
- `test_storage_entry_persistence` - Entry and HardState durability
- `test_storage_compaction` - Log compaction correctness
- `test_storage_entries_max_size` - Batch size limiting

**gRPC Tests** (`tests/grpc_test.rs` - 303 lines):
- `test_raft_service_receive_message` - End-to-end message delivery
- `test_peer_registry_operations` - Add/get/remove peers
- `test_peer_registry_from_config` - Bulk initialization
- `test_multiple_messages` - Sequential message handling
- `test_connection_pooling` - Efficiency (20 messages in ~100ms)
- `test_unknown_peer_error` - Error handling for invalid destinations
- `test_start_raft_server` - Convenience function validation

**Backward Compatibility**:
- Updated raft_simulation.rs to use `new_memory()`

### Build System ✅

- `build.rs` compiles both federation.proto and raft.proto
- `rpc.rs` includes both protocol buffers
- Version alignment: tonic 0.8 + prost 0.11 (matches raft 0.7.0)

### Quality Metrics ✅

- ✅ `cargo fmt`: Perfect formatting
- ✅ `cargo clippy`: Zero warnings
- ✅ `cargo test`: All tests passing (42 total in federation crate)
- ✅ `cargo build`: Clean build
- ⚠️ Security: RUSTSEC-2024-0437 mitigated

### Production Usage

```rust
use federation::{RaftEngine, RaftEngineConfig, PeerRegistry, RaftTransportClient, start_raft_server};
use std::sync::Arc;

// 1. Open persistent storage
let storage_path = "/var/lib/space/raft";
let config = RaftEngineConfig { id: 1, peers: vec![1, 2, 3] };

// 2. Create channels
let (inbox_tx, inbox_rx) = mpsc::channel(100);
let (outbox_tx, mut outbox_rx) = mpsc::channel(100);
let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

// 3. Create engine with persistent storage
let engine = RaftEngine::new_persistent(config, storage_path, inbox_rx, outbox_tx, shutdown_rx)?;

// 4. Start gRPC server (receives messages → inbox_tx)
tokio::spawn(start_raft_server("127.0.0.1:4422".parse()?, inbox_tx));

// 5. Start transport client (sends outbox messages via gRPC)
let registry = PeerRegistry::from_config(&[
    (1, "http://127.0.0.1:4422"),
    (2, "http://127.0.0.1:4423"),
    (3, "http://127.0.0.1:4424"),
]);
let client = RaftTransportClient::new(Arc::new(registry));
tokio::spawn(async move {
    while let Some((to, msg)) = outbox_rx.recv().await {
        if let Err(e) = client.send(to, msg).await {
            error!("Failed to send message: {}", e);
        }
    }
});

// 6. Run engine
engine.run().await?;
```

### Remaining Limitations

- **Fixed Membership**: Cannot add/remove nodes dynamically (Phase 9.3)
- **No State Machine**: Commits logged but not applied to application state (Phase 9.3)

### Next Steps

Phase 9.3 will add state machine integration with FederationBridge and dynamic membership changes.

## Phase 9.3: Federation Integration 🟡 PLANNED

**Target**: Q2 2025

### Goals

Wire RaftEngine into FederationBridge for zone coordination and dynamic membership.

### Implementation Plan

1. **Zone Leader Election**
   - Use Raft to elect zone leaders
   - Leaders coordinate capsule placement
   - Followers redirect to leader
   - Leader lease mechanism

2. **Routing Table Management**
   - Raft consensus for routing updates
   - Volume → Node mappings in state machine
   - Consistent routing across cluster
   - Routing cache invalidation

3. **Dynamic Membership**
   - Add/remove nodes via Raft membership changes
   - Joint consensus for safe reconfiguration
   - Automatic discovery via gossip layer
   - Health-based membership decisions

4. **Integration Points**
   ```rust
   impl FederationBridge {
       async fn apply_routing_change(&self, change: RoutingChange) {
           // Propose to Raft
           self.raft_engine.propose(serialize(change)).await?;
       }

       fn on_committed(&self, entry: LogEntry) {
           // Apply to local routing table
           self.routing_table.apply(entry)?;
       }
   }
   ```

### Deliverables

- ✅ RaftEngine + FederationBridge integration
- ✅ Zone leader election mechanism
- ✅ Routing table consensus
- ✅ Dynamic membership (add/remove nodes)
- ✅ Tests for membership changes and routing

## Phase 9.4: Network Transport 🟡 PLANNED

**Target**: Q3 2025

### Goals

Replace in-process mpsc channels with gRPC for cross-process Raft clusters.

### Implementation Plan

1. **gRPC Transport**
   - Define Raft RPC service in protobuf
   - AppendEntries, RequestVote, InstallSnapshot RPCs
   - Connection pooling and retry logic
   - TLS/mTLS for secure communication

2. **Protocol Definition**
   ```protobuf
   service RaftTransport {
       rpc AppendEntries(AppendEntriesRequest) returns (AppendEntriesResponse);
       rpc RequestVote(RequestVoteRequest) returns (RequestVoteResponse);
       rpc InstallSnapshot(stream SnapshotChunk) returns (SnapshotResponse);
   }
   ```

3. **Network Layer**
   - Replace mpsc channels with gRPC clients
   - Async message sending with backpressure
   - Connection health monitoring
   - Graceful reconnection on failures

4. **Configuration**
   - Cluster configuration file (peers, addresses)
   - Bootstrap process for new nodes
   - Auto-discovery via existing gossip layer
   - Certificate management for mTLS

### Deliverables

- ✅ gRPC-based Raft transport
- ✅ Cross-process Raft cluster support
- ✅ TLS/mTLS security
- ✅ Production deployment readiness
- ✅ Multi-node integration tests

## Phase 9.5: Advanced Features 🟡 PLANNED

**Target**: Q4 2025

### Goals

Production hardening with advanced Raft features.

### Implementation Plan

1. **Log Compaction**
   - Automatic log truncation after snapshots
   - Configurable retention policy
   - Space reclamation
   - Performance optimization

2. **Learner Nodes**
   - Read-only replicas for scaling reads
   - Non-voting members
   - Async replication to learners
   - Promotion to full members

3. **Pre-Vote**
   - Prevent unnecessary elections
   - Reduce election storms in partitions
   - Better leader stability
   - Lower network overhead

4. **Joint Consensus**
   - Safe membership reconfiguration
   - Two-phase membership changes
   - Prevents split-brain scenarios
   - Atomic configuration updates

5. **Metrics & Observability**
   - Raft metrics (term, commit index, apply index)
   - Leadership duration tracking
   - Election frequency monitoring
   - Latency percentiles
   - Prometheus integration

### Deliverables

- ✅ Log compaction and GC
- ✅ Learner node support
- ✅ Pre-vote optimization
- ✅ Joint consensus for membership changes
- ✅ Comprehensive metrics and monitoring

## Data Flow Example

### Scenario: CreateVolume Request

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. Client sends CreateVolume to Any Node                        │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. Node forwards request to Raft Leader (if not leader)         │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. Leader appends CreateVolume to Raft Log                      │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 4. Cluster replicates Log via AppendEntries                     │
│    (Leader → Followers in parallel)                             │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 5. Commit: Once N/2 + 1 nodes acknowledge                       │
│    Leader marks entry as committed                              │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 6. Apply: All nodes apply CreateVolume to State Machine         │
│    Updates: Registry, Routing Table, Telemetry                  │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│ 7. Leader notifies client of success                            │
└─────────────────────────────────────────────────────────────────┘
```

## Configuration Example

### Phase 9.4+ Cluster Config

```yaml
# raft-cluster.yaml
cluster:
  name: "space-prod"
  nodes:
    - id: 1
      addr: "10.0.1.10:5000"
      zone: "us-east-1a"
    - id: 2
      addr: "10.0.1.11:5000"
      zone: "us-east-1b"
    - id: 3
      addr: "10.0.1.12:5000"
      zone: "us-west-2a"

raft:
  election_tick: 10        # 1s timeout
  heartbeat_tick: 3        # 300ms heartbeat
  snapshot_interval: 10000 # Snapshot every 10k entries
  log_retention: 50000     # Keep 50k entries post-snapshot

transport:
  protocol: grpc
  tls:
    enabled: true
    cert_path: /etc/space/certs/server.crt
    key_path: /etc/space/certs/server.key
    ca_path: /etc/space/certs/ca.crt

storage:
  backend: sled
  path: /var/lib/space/raft
  fsync: true
```

## Testing Strategy

### Phase 9.1 (Complete)
- ✅ 3-node simulation with in-process channels
- ✅ Leader election verification
- ✅ Graceful shutdown

### Phase 9.2
- Persistence across restarts
- Snapshot creation and recovery
- Log compaction effectiveness

### Phase 9.3
- Dynamic membership (add/remove nodes)
- Routing table consistency
- Zone leader election

### Phase 9.4
- Cross-process cluster
- Network partition resilience
- TLS/mTLS security

### Phase 9.5
- Chaos engineering (random failures)
- Performance benchmarking
- Multi-datacenter scenarios

## Performance Characteristics

### Expected Latency (Phase 9.4+)

- **Leader Election**: ~1-2 seconds (typical)
- **Commit Latency**: ~50-100ms (intra-zone)
- **Commit Latency**: ~200-500ms (cross-zone)
- **Snapshot Creation**: ~1-5 seconds (depends on state size)

### Scalability

- **Cluster Size**: 3-7 nodes recommended (Raft best practice)
- **Throughput**: ~10,000 ops/sec (leader bottleneck)
- **Log Size**: Compaction keeps <100MB typically

## Migration Path

### From Phase 9.1 to 9.2
1. Stop cluster (no persistence in 9.1)
2. Upgrade binaries with sled storage
3. Bootstrap with empty state
4. Restore from backup if needed

### From 9.2 to 9.3+
- Rolling upgrade supported
- No state migration needed
- Backward compatible protocol

## Known Issues & Limitations

### Phase 9.1
- ✅ protobuf 2.28.0 DoS vulnerability (raft dependency) - mitigated
  - Acknowledged in `.cargo/audit.toml` with risk assessment
  - Future: Phase 9.2 will upgrade to raft 0.8+ or fork raft-proto

### Future Considerations
- Raft is CP (Consistency + Partition tolerance), sacrifices Availability during partitions
- Leader is a write bottleneck (single writer)
- Cross-datacenter deployments may have higher latency

## References

- [Raft Consensus Algorithm](https://raft.github.io/) - Official spec
- [tikv/raft-rs](https://github.com/tikv/raft-rs) - Implementation used
- [SPACE Federation Docs](federation.md) - Integration details
- [Federation README](../crates/federation/README.md) - API reference

## See Also

- [Phase 3: Mesh Cluster](phase3.md) - Gossip protocol foundation
- [Phase 4: Views & Federation](phase4.md) - Data plane replication
- [Architecture Overview](architecture.md) - Overall system design
- [CHANGELOG](../CHANGELOG.md) - Version history

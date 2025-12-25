# Phase 9: Federation Control Plane

**Status**: 🟢 Phase 9.1 Complete | 🟡 Phase 9.2-9.5 In Progress

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
- ⚠️ `cargo audit`: 1 known issue (documented for 9.2)

### Phase 9.1 Limitations (By Design)

- **No Persistence**: Uses MemStorage (lost on restart)
- **In-Process Only**: Test uses mpsc channels, not real network
- **Fixed Membership**: Cannot add/remove nodes dynamically
- **No State Machine**: Commits are logged but not applied

## Phase 9.2: Persistence & State Machine 🟡 PLANNED

**Target**: Q1 2025

### Goals

Replace in-memory storage with durable persistence and implement state machine application.

### Implementation Plan

1. **Persistent Storage**
   - Replace MemStorage with sled or rocksdb
   - WAL (Write-Ahead Log) for committed entries
   - Atomic fsync for durability guarantees
   - Snapshot support for faster recovery

2. **State Machine Application**
   - Define state machine trait for control plane metadata
   - Apply committed entries to cluster state
   - Track applied index for crash recovery
   - Idempotent operations (replay safety)

3. **State Machine Operations**
   ```rust
   enum ControlPlaneOp {
       CreateVolume { id: VolumeId, node: NodeId },
       UpdateRouting { volume: VolumeId, replicas: Vec<NodeId> },
       NodeJoin { id: NodeId, addr: SocketAddr },
       NodeLeave { id: NodeId },
       ZoneUpdate { id: ZoneId, config: ZoneConfig },
   }
   ```

4. **Snapshot Implementation**
   - Periodic snapshot creation (e.g., every 10,000 entries)
   - Snapshot transfer to followers
   - Log compaction after snapshot
   - Snapshot metadata tracking

### Deliverables

- ✅ sled-based RaftStorage implementation
- ✅ ControlPlaneStateMachine trait and implementation
- ✅ Snapshot creation and application
- ✅ Recovery from snapshots + log replay
- ✅ Tests for persistence and crash recovery

### Security Fix

- Resolve RUSTSEC-2024-0437 (protobuf 2.28.0 DoS)
- Options: Upgrade raft to protobuf 3.x compatible version, or fork raft-proto

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
- ⚠️ protobuf 2.28.0 DoS vulnerability (raft dependency)
  - Resolution: Phase 9.2 upgrade or fork
  - Risk: Low (DoS only, dev environment)

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

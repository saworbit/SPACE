# Federation Crate

Multi-zone data replication and distributed consensus for SPACE.

## Overview

The `federation` crate provides two main capabilities:

1. **Control Plane Consensus** (Phase 9.1) - Raft-based cluster coordination
2. **Data Plane Replication** (Phase 4b) - gRPC-based inter-zone replication

## Phase 9.1: Raft Consensus Engine (Complete)

### Quick Start (In-Memory)

```rust
use federation::{RaftEngine, RaftEngineConfig};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create channels
    let (inbox_tx, inbox_rx) = mpsc::channel(100);
    let (outbox_tx, outbox_rx) = mpsc::channel(100);
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

    // Configure Raft engine
    let config = RaftEngineConfig {
        id: 1,                  // Node ID (unique in cluster)
        peers: vec![1, 2, 3],   // All node IDs including self
    };

    // Create in-memory engine (testing/development)
    let engine = RaftEngine::new_memory(config, inbox_rx, outbox_tx, shutdown_rx)?;

    // Spawn the event loop
    tokio::spawn(async move {
        engine.run().await
    });

    // Propose commands (only succeeds if leader)
    let data = b"CreateVolume:Vol-X".to_vec();
    engine.propose(data).await?;

    // Check leadership
    if engine.is_leader() {
        println!("I am the leader at term {}", engine.current_term());
    }

    Ok(())
}
```

## Phase 9.2: Persistence & Transport (Complete) ✅ NEW

### Production Deployment with Persistence

```rust
use federation::{RaftEngine, RaftEngineConfig, SledStorage};
use std::path::Path;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let storage_path = Path::new("/var/lib/space/raft");

    // Create channels
    let (inbox_tx, inbox_rx) = mpsc::channel(100);
    let (outbox_tx, outbox_rx) = mpsc::channel(100);
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

    // Configure Raft engine
    let config = RaftEngineConfig {
        id: 1,
        peers: vec![1, 2, 3],
    };

    // Create persistent engine (production)
    let engine = RaftEngine::new_persistent(
        config,
        storage_path,
        inbox_rx,
        outbox_tx,
        shutdown_rx
    )?;

    // Spawn the event loop
    tokio::spawn(async move {
        engine.run().await
    });

    // Data survives restarts! 🎉
    engine.propose(b"CreateVolume:Vol-X".to_vec()).await?;

    Ok(())
}
```

### Network Transport with gRPC

```rust
use federation::{start_raft_server, PeerRegistry, RaftTransportClient};
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (inbox_tx, mut inbox_rx) = mpsc::channel(100);
    let addr: SocketAddr = "127.0.0.1:4422".parse()?;

    // Start gRPC server (receives messages)
    tokio::spawn(start_raft_server(addr, inbox_tx));

    // Configure peer registry
    let registry = PeerRegistry::from_config(&[
        (1, "http://127.0.0.1:4422"),
        (2, "http://127.0.0.1:4423"),
        (3, "http://127.0.0.1:4424"),
    ]);

    // Create transport client (sends messages)
    let client = RaftTransportClient::new(Arc::new(registry));

    // Send Raft messages over the network
    let msg = raft::prelude::Message {
        msg_type: raft::prelude::MessageType::MsgHeartbeat as i32,
        from: 1,
        to: 2,
        term: 5,
        ..Default::default()
    };

    client.send(2, msg).await?;

    Ok(())
}
```

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Federation Crate                         │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ┌────────────────────┐        ┌─────────────────────────┐ │
│  │  Control Plane     │        │  Data Plane             │ │
│  │  (Phase 9.1)       │        │  (Phase 4b)             │ │
│  ├────────────────────┤        ├─────────────────────────┤ │
│  │ RaftEngine         │        │ FederationBridge        │ │
│  │  - Leader election │        │  - Capsule replication  │ │
│  │  - Cluster state   │        │  - Segment transfer     │ │
│  │  - Consensus       │        │  - Queue management     │ │
│  └────────────────────┘        └─────────────────────────┘ │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### Key Features

- **Automatic Leader Election**: Cluster self-heals when nodes fail (1s timeout)
- **Consensus Protocol**: Industry-standard Raft from tikv/raft-rs v0.7.0
- **Async-Friendly**: Full tokio integration with proper Send trait compliance
- **Production Ready**: Comprehensive testing and error handling

### Testing

Run the 3-node simulation:

```bash
# Basic test
cargo test -p federation --test raft_simulation

# With detailed logs
RUST_LOG=info cargo test -p federation --test raft_simulation -- --nocapture
```

Expected output:
```
INFO federation::engine: created raft engine id=1 peers=[1, 2, 3]
INFO federation::engine: starting raft engine event loop id=1
INFO raft_simulation: router: starting
... [election happens] ...
INFO raft_simulation: Election phase complete
```

### Phase 9.2 Features ✅ COMPLETE

- **✅ Persistent Storage**: SledStorage backed by embedded database
  - Survives restarts with full state recovery
  - Separate trees for hard_state, conf_state, entries, snapshots
  - Atomic fsync for durability guarantees
  - Log compaction support via `compact()` method

- **✅ Network Transport**: gRPC-based message passing
  - Cross-process Raft clusters
  - Connection pooling for efficiency
  - PeerRegistry for endpoint management
  - Prost serialization over HTTP/2

- **✅ Generic Storage**: Engine works with any Storage implementation
  - `new_memory()` - MemStorage for testing
  - `new_persistent()` - SledStorage for production
  - Easy to add custom backends

- **✅ Comprehensive Testing**:
  - Persistence across restarts verified
  - gRPC transport end-to-end tests
  - Connection pooling performance validated
  - Backward compatibility maintained

### Remaining Limitations

- **Fixed Membership**: Cannot add/remove nodes dynamically
  - Phase 9.3 will add membership changes via joint consensus
- **No State Machine**: Commits are logged but not applied to application state
  - Phase 9.3 will add state machine integration with FederationBridge

### API Reference

#### `RaftEngineConfig`

```rust
pub struct RaftEngineConfig {
    pub id: u64,           // Unique node ID
    pub peers: Vec<u64>,   // All peer IDs (including self)
}
```

#### `RaftEngine`

**Methods:**

- `new(config, inbox, outbox, shutdown) -> Result<Self>`
  - Creates a new Raft engine instance

- `async run(self) -> Result<()>`
  - Main event loop (runs until shutdown)
  - Handles ticks, messages, and ready state processing

- `async propose(&self, data: Vec<u8>) -> Result<()>`
  - Proposes a command to the cluster
  - Only succeeds if this node is the leader

- `is_leader(&self) -> bool`
  - Returns true if this node is currently the leader

- `current_term(&self) -> u64`
  - Returns the current Raft term

- `leader_id(&self) -> Option<u64>`
  - Returns the current leader's node ID (if known)

### Configuration

**Raft Timings:**
- Tick interval: 100ms
- Election timeout: 10 ticks (1 second)
- Heartbeat interval: 3 ticks (300ms)

These are production-proven values from TiKV/Etcd.

## Phase 4b: Data Plane Replication

See [docs/federation.md](../../docs/federation.md) for documentation on:
- FederationBridge
- WAN replication
- Zone configuration
- Capsule transfer

## Architecture Notes

### Two Raft Systems

SPACE uses **two separate Raft implementations**:

1. **capsule-registry Raft** (openraft 0.9.21)
   - Purpose: Metadata consensus within a zone
   - Location: `crates/capsule-registry/src/mesh.rs`

2. **federation Raft** (tikv/raft-rs 0.7.0) ⭐ THIS CRATE
   - Purpose: Control plane consensus across zones
   - Location: `crates/federation/src/engine.rs`

They operate independently and serve different purposes.

### Why tikv/raft-rs?

- **Battle-tested**: Used in TiKV, a distributed database serving production workloads
- **Performance**: Optimized for high-throughput, low-latency consensus
- **Ecosystem**: Compatible with etcd's Raft implementation
- **Maturity**: Well-documented with extensive testing

## Future Roadmap

### ✅ Phase 9.2: Persistence & Transport (COMPLETE)
- ✅ SledStorage with persistent state
- ✅ gRPC transport layer
- ✅ Connection pooling
- ✅ Log compaction support
- ✅ Snapshot infrastructure
- ✅ Generic storage trait

### Phase 9.3: Federation Integration (Planned)
- Wire RaftEngine into FederationBridge
- Use Raft for zone leader election
- Coordinate zone routing changes via consensus
- Dynamic cluster membership with joint consensus
- State machine application for cluster metadata

### Phase 9.4: Advanced Features (Planned)
- TLS/mTLS for secure communication
- Learner nodes for scaling reads
- Pre-vote to prevent election storms
- Automatic log compaction and garbage collection
- Metrics and observability integration

### Phase 9.5: Production Hardening (Planned)
- Chaos engineering and failure testing
- Performance optimization and benchmarking
- Multi-datacenter deployment patterns
- Advanced monitoring and alerting

## Contributing

See [docs/implementation/CONTRIBUTING.md](../../docs/implementation/CONTRIBUTING.md) for development guidelines.

## License

Apache-2.0

## References

- [Raft Consensus Algorithm](https://raft.github.io/)
- [tikv/raft-rs Documentation](https://docs.rs/raft/latest/raft/)
- [SPACE Federation Guide](../../docs/federation.md)
- [Phase 9 Specification](../../docs/phase9.md) (coming soon)

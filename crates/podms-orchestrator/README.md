# PODMS Orchestrator

Multi-node coordination layer for SPACE's distributed storage system.

## Overview

The PODMS Orchestrator provides the coordination layer that wires together all multi-node components into a cohesive distributed system. It manages:

- **Gossip Layer**: Epidemic state propagation for metadata and events
- **Mesh Networking**: P2P connectivity and data replication
- **Scaling Agent**: Autonomous execution of scaling actions
- **Reconciler**: Self-driving control loop for volume management (Phase 9.4+)
  - Volume creation and deletion (Phase 9.4)
  - Snapshot-based volume hydration (Phase 9.6)
- **Telemetry Bus**: Event-driven coordination across components

## Components

### Reconciler (Phase 9.4+)

The **Reconciler** is the "Nervous System" that connects the Federation Registry (Brain) with the Foundry storage engine (Muscle). It implements a continuous control loop that ensures local storage state matches the desired global state, including automatic volume hydration from snapshots (Phase 9.6).

#### Architecture

```
┌─────────────────────┐
│ Federation Registry │ ← Brain (What SHOULD exist)
│   (Raft Consensus)  │
└──────────┬──────────┘
           │ get_state()
           ↓
┌─────────────────────┐
│    Reconciler       │ ← Nervous System (Converges state)
│  (This Component)   │
└──────────┬──────────┘
           │ create_volume() / delete_volume()
           ↓
┌─────────────────────┐
│   Foundry Engine    │ ← Muscle (What ACTUALLY exists)
│  (Local Storage)    │
└─────────────────────┘
```

#### Control Loop

The reconciler runs a continuous loop (default: every 5 seconds) that:

1. **Observe**: Fetches desired state from Federation Registry
2. **Filter**: Extracts volumes assigned to this node
3. **Diff**: Compares with actual Foundry state
4. **Act**: Creates missing volumes, deletes zombie volumes

#### Usage

```rust
use std::sync::Arc;
use capsule_registry::CapsuleRegistry;
use capsule_registry::pipeline::WritePipeline;
use foundry::Foundry;
use foundry::snapshot::SnapshotEngine;
use federation::Registry;
use nvram_sim::NvramLog;
use podms_orchestrator::Reconciler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup components
    let foundry = Arc::new(Foundry::new());
    let registry = Arc::new(Registry::new());

    // Setup snapshot engine for hydration (Phase 9.6)
    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open("data/nvram.log")?;
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

    let node_id = 1;

    // Create reconciler with snapshot engine
    let reconciler = Reconciler::new(node_id, foundry, registry, snapshot_engine)
        .with_interval(std::time::Duration::from_secs(10)); // Optional custom interval

    // Run continuously in background
    tokio::spawn(async move {
        reconciler.run().await;
    });

    // Main application logic continues...

    Ok(())
}
```

#### Features

- **Self-Driving**: Automatically creates volumes when they appear in Registry
- **Volume Hydration** (Phase 9.6): Automatically restores volumes from snapshots
  - Detects `source_capsule_id` in volume metadata
  - Calls SnapshotEngine to restore data from capsule registry
  - Handles failures gracefully with automatic cleanup
- **Self-Healing**: Automatically removes zombie volumes not in Registry
- **Graceful Recovery**: Never crashes - logs errors and continues
- **Thread-Safe**: Uses Arc/RwLock for concurrent operation
- **Configurable**: Adjustable reconciliation interval
- **Idempotent**: Safe to run multiple times

#### Volume ID Format

The Reconciler requires volume IDs in the Registry to be valid UUIDs. This ensures compatibility between the Registry (which uses String IDs) and Foundry (which uses VolumeId(UUID)).

**Example valid volume ID**: `550e8400-e29b-41d4-a716-446655440000`

#### Error Handling

The reconciler implements robust error handling:

- **Parse errors**: Invalid UUID strings are logged and skipped
- **Foundry errors**: Volume creation/deletion failures are logged and retried next cycle
- **Registry errors**: State fetch errors are logged and retried next cycle

The loop never panics and continues operating even if individual operations fail.

## Configuration

The orchestrator can be configured via YAML:

```yaml
node_id: "node-1"
listen_addr: "0.0.0.0:4421"
zone_name: "us-west-1a"
default_policy:
  replication: 3
  compression: "zstd"
seed_peers:
  - "10.0.1.10:4421"
  - "10.0.1.11:4421"
```

See `OrchestratorConfig` for all available options.

## Testing

The crate includes comprehensive integration tests:

```bash
# Run all tests
cargo test -p podms-orchestrator

# Run reconciler tests specifically
cargo test -p podms-orchestrator --test reconciler_test

# Run with logging
RUST_LOG=info cargo test -p podms-orchestrator --test reconciler_test
```

## Dependencies

- **foundry**: Block storage engine for local volumes
- **federation**: Raft-based distributed registry
- **mesh-core**: P2P mesh networking
- **gossip-layer**: Epidemic state propagation
- **scaling**: Autonomous scaling agent

## Architecture Notes

### Reconciler Design Decisions

1. **Standalone Component**: The Reconciler is independent from the existing Orchestrator for simpler testing and deployment
2. **UUID Enforcement**: Volume IDs must be valid UUIDs for type safety
3. **Aggressive Cleanup**: Zombie volumes are deleted automatically with safety logging
4. **Auto Backend**: Uses `BackendType::Auto` for maximum compatibility

### Integration Points

- **Federation Registry**: Source of truth for cluster topology via `get_state()`
- **Foundry Engine**: Local storage backend via `list_volumes()`, `create_volume()`, `delete_volume()`
- **Logging**: Structured logging via `tracing` for observability

## Phase History

- **Phase 9.4** ✅: Node Reconciliation - Self-driving volume creation/deletion
- **Phase 9.5** ✅: Placement Scheduler - Intelligent node selection
- **Phase 9.6** ✅: Volume Hydration - Snapshot-based volume restoration
  - Automatic hydration from `source_capsule_id`
  - Cleanup on failure (idempotent retry)
  - Integration with SnapshotEngine

## Future Roadmap

- **Phase 9.7**: Health Checks - Monitor volume health and report to Registry
- **Phase 10+**: Advanced Features - Progress tracking, incremental hydration, cross-zone restore

## License

Dual-licensed under MIT or Apache 2.0.

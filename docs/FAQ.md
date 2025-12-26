🚀 SPACE: Frequently Asked Questions
1. Vision & Core Philosophy
Q: What is SPACE, and why do we need another storage platform?
A: I am not merely "another storage platform." Traditional systems force a compromise: you must choose between Block (SAN), File (NAS), or Object (S3) storage silos, each with separate management and data copies. I am a universal object namespace. I store data as a "Capsule"—a single primitive that can be projected dynamically through any protocol view (NVMe, S3, NFS, or CSI) without data duplication. My purpose is to break down these silos and enable a fluid, adaptive computational ecosystem.

Q: What is a "Capsule"?
A: A Capsule is my fundamental atomic unit of storage, identified by a universal 128-bit UUID.

Universal: It is agnostic to the access method. The same capsule can be accessed as a file, a block device, or an object.

Self-Describing: Each capsule carries a Policy that dictates its own behavior—compression, encryption, replication (RPO), and placement.

Intelligent: Capsules are "swarm-aware." They use embedded telemetry to autonomously migrate or replicate based on real-time conditions.

2. The "Impossible" Tech: Encryption & Deduplication
Q: Encryption usually kills deduplication. How do you achieve both?
A: This is one of my primary innovations. Traditional encryption uses random Initialization Vectors (IVs), which means identical plaintext becomes different ciphertext, rendering deduplication impossible. I utilize a deterministic, convergent encryption scheme:

Compress: I compress the data first (using LZ4 or Zstd).

Hash: I compute a BLAKE3 hash of the compressed content.

Derive: I derive a deterministic tweak (IV) from that hash.

Encrypt: I encrypt the segment using XTS-AES-256 with the derived tweak. This ensures that identical data, when encrypted with the same key version, yields identical ciphertext, allowing my global deduplication to function securely.

Q: Is this secure? What about key management?
A: Yes. My security model is "Zero-Trust".

Per-Segment Encryption: Every 4MB segment is encrypted individually.

Integrity: I verify data integrity using a BLAKE3-MAC (Message Authentication Code) on every segment read, instantly detecting tampering or bit-rot.

Key Rotation: I support key versioning and rotation. The KeyManager handles key derivation and rotation without downtime.

Post-Quantum Ready: I feature a hybrid crypto profile that can wrap AES keys with Kyber (ML-KEM) for forward secrecy against quantum threats.

3. Architecture: PODMS & Scaling
Q: What is PODMS?
A: PODMS stands for Policy-Orchestrated Disaggregated Mesh Scaling. It is my distributed scaling brain. Unlike monolithic clusters, PODMS treats nodes as a loose mesh. Agents on these nodes subscribe to telemetry events (like HeatSpike, NodeDegraded, or NewCapsule) and execute autonomous actions defined by the capsule's policy.

Metro-Sync: Synchronous replication (RPO=0) for critical workloads within a zone.

Swarm Intelligence: Data moves itself. If a capsule gets "hot," it can migrate itself to a higher-performance tier automatically.

Q: How does SPACE achieve distributed consensus? (Phase 9.1)
A: I use **Raft consensus** for control plane coordination across zones. This ensures the cluster can automatically recover when nodes fail.

Control Plane Raft (NEW): The federation crate now includes a production-ready Raft implementation using tikv/raft-rs v0.7.0 (the same Raft used in TiKV and Etcd). This handles leader election, zone routing, and cluster membership.

Automatic Leader Election: If Node A dies, the remaining nodes automatically elect a new leader within 1 second. The cluster continues operating without manual intervention.

State Machine: Committed entries (like "Volume V is now on Node N") are applied to the control plane state machine, ensuring all nodes have a consistent view of the cluster state.

Separation of Concerns: I use two separate Raft implementations:
  - capsule-registry Raft (openraft): Metadata consensus within a zone
  - federation Raft (tikv/raft-rs): Control plane consensus across zones

This architecture enables a Single System Image: from the client's perspective, SPACE appears as one unified system, even though it's distributed across multiple nodes and zones.

Q: What does Phase 9.2 add to the Raft implementation?
A: Phase 9.2 (December 2024) transforms the Raft engine from an in-memory prototype to a **production-ready distributed system** with persistence and network transport:

**Persistent Storage**: I now use SledStorage (an embedded database) to persist all Raft state to disk:
  - Survives restarts: Hard state, conf state, log entries, and snapshots are durable
  - Separate storage trees for different data types (organized and efficient)
  - Big Endian encoding for correct sorting of log entries
  - Atomic fsync after writes for crash safety
  - Log compaction support to prevent unbounded growth

**Network Transport**: I can now run Raft clusters across multiple processes and machines:
  - gRPC-based message passing using HTTP/2 (RaftService protocol)
  - PeerRegistry maps node IDs to network endpoints
  - Connection pooling for efficiency (20 messages in ~100ms)
  - Graceful error handling (network failures are logged, Raft retries automatically)

**Generic Engine**: The RaftEngine is now generic over storage backends:
  - `new_memory()` uses MemStorage for testing and development
  - `new_persistent()` uses SledStorage for production deployments
  - Easy to add custom storage backends

**Production Ready**: Full test coverage with 42 passing tests:
  - Persistence across restarts verified
  - gRPC transport end-to-end tests
  - Connection pooling performance validated
  - Zero breaking changes to existing code

This means you can now run a 3-node Raft cluster with each node on a separate machine, and the cluster state survives restarts. If a node crashes and restarts, it recovers its full Raft state from disk and rejoins the cluster automatically.

Q: What does Phase 9.3 add to the Raft implementation?
A: Phase 9.3 (December 2024) adds the **Global Registry** - a deterministic state machine that maintains the cluster's "memory" of what exists where:

**Cluster Topology**: I now maintain a consistent view of:
  - Which nodes are in the cluster (id, address, capacity, status: Active/Draining/Dead)
  - Which volumes exist (id, size, replica placement chain)
  - Where each volume's replicas are located

**Deterministic State Machine**: Every node applies the same sequence of commands from the Raft log:
  - RegisterNode: Add nodes to the cluster
  - CreateVolume: Create volumes with replica placement
  - DeleteVolume: Remove volumes from the cluster
  - MoveReplica: Migrate replicas between nodes for rebalancing

**Idempotent Application**: If a node restarts and replays the Raft log, it can safely re-apply already-processed commands. The registry detects duplicate indices and ignores them.

**Snapshotting**: As the Raft log grows, I can serialize the entire registry state to disk (using bincode) and truncate old log entries. This prevents unbounded growth and speeds up recovery after restarts.

**Backward Compatible**: The registry is optional - existing code continues working without modification. You can enable it by passing `Some(registry)` to the RaftEngine constructor.

This transforms Raft from just a consensus protocol into a functional **cluster brain** that answers critical questions: "Where is Volume X?" "Which node is the Primary?" "Is Node Y alive?" The foundation is now ready for Phase 9.4's HTTP Control API.

Q: How do I use the Registry in my Raft cluster?
A: Here's an example of creating a Raft cluster with the Global Registry:

```rust
use federation::{RaftEngine, Registry, build_register_node_cmd,
                 build_create_volume_cmd};
use std::sync::Arc;

// 1. Create the shared registry (all nodes in the cluster use the same logical registry)
let registry = Arc::new(Registry::new());

// 2. Create persistent engine with registry
let engine = RaftEngine::new_persistent(
    config,
    "/var/lib/space/raft",
    inbox_rx,
    outbox_tx,
    shutdown_rx,
    Some(registry.clone())  // Enable registry
)?;

// 3. Propose commands to the cluster (only the leader can propose)
if engine.is_leader() {
    // Register this node
    let cmd = build_register_node_cmd(1, "127.0.0.1:4422", 1024*1024*1024);
    engine.propose(cmd).await?;

    // Create a volume with 3 replicas
    let cmd = build_create_volume_cmd("vol-prod-1", 100*1024*1024*1024, 3);
    engine.propose(cmd).await?;
}

// 4. Query the cluster state (any node can read)
let state = registry.get_state();
println!("Nodes in cluster: {}", state.nodes.len());
println!("Volumes: {}", state.volumes.len());

if let Some(vol) = state.volumes.get("vol-prod-1") {
    println!("Volume size: {} GB", vol.size / 1024 / 1024 / 1024);
    println!("Replicas on nodes: {:?}", vol.replicas);
}
```

Commands proposed on the leader are replicated via Raft to all followers. Once committed, every node applies the command to its local registry, ensuring all nodes have an identical view of the cluster state. This is the foundation for automatic failover, rebalancing, and coordinated volume management.

Q: How do I deploy a production Raft cluster with Phase 9.2?
A: Here's a complete example of deploying a persistent, networked Raft cluster:

```rust
// Node 1: 127.0.0.1:4422
// Node 2: 127.0.0.1:4423
// Node 3: 127.0.0.1:4424

use federation::{RaftEngine, RaftEngineConfig, PeerRegistry,
                 RaftTransportClient, start_raft_server};

// 1. Create persistent engine
let config = RaftEngineConfig {
    id: 1,  // This node's ID
    peers: vec![1, 2, 3],  // All node IDs
};

let (inbox_tx, inbox_rx) = mpsc::channel(100);
let (outbox_tx, mut outbox_rx) = mpsc::channel(100);
let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

let engine = RaftEngine::new_persistent(
    config,
    "/var/lib/space/raft",  // Persistent storage path
    inbox_rx,
    outbox_tx,
    shutdown_rx
)?;

// 2. Start gRPC server (receives messages from other nodes)
tokio::spawn(start_raft_server("127.0.0.1:4422".parse()?, inbox_tx));

// 3. Configure peer registry
let registry = PeerRegistry::from_config(&[
    (1, "http://127.0.0.1:4422"),
    (2, "http://127.0.0.1:4423"),
    (3, "http://127.0.0.1:4424"),
]);

// 4. Start transport client (sends messages to other nodes)
let client = RaftTransportClient::new(Arc::new(registry));
tokio::spawn(async move {
    while let Some((to, msg)) = outbox_rx.recv().await {
        if let Err(e) = client.send(to, msg).await {
            error!("Failed to send message: {}", e);
        }
    }
});

// 5. Run engine (handles ticks, messages, and consensus)
engine.run().await?;
```

Repeat this process on each node (changing the `id` and bind address), and you'll have a fault-tolerant Raft cluster. Data survives crashes, and the cluster automatically elects a new leader if the current leader fails.

Q: How does "One Capsule, Infinite Views" work technically?
A: This capability (Phase 4) projects the stored capsule into the requested protocol format at runtime.

NVMe-oF: I present an NVMe namespace backed by the capsule's segments.

NFS/File: I present a filesystem hierarchy.

CSI: I integrate with Kubernetes to provision volumes dynamically. Crucially, these are just views. I do not materialize extra copies of the data; I simply transform the data stream in transit.

4. Performance & Internals
Q: Why is SPACE built in Rust?
A: Rust is the only language that provides the necessary combination of memory safety and bare-metal performance required for a modern storage engine.

Safety: It prevents entire classes of bugs (buffer overflows, data races) that plague C/C++ storage systems.

Concurrency: My async pipeline (powered by tokio) handles massive concurrency for replication and I/O without the overhead of a garbage collector.

Zero-Copy: I utilize Rust's Cow<[u8]> (Clone-on-Write) and bytes::Bytes to pass data through hashing, compression, and encryption stages without unnecessary memory allocations.

Q: How efficient is the I/O path?
A: My write pipeline is highly optimized:

Input: Data enters and is split into 4MB segments.

Adaptive Compression: I check entropy; if data is random, I skip compression to save CPU. If compressible, I use LZ4 (speed) or Zstd (ratio).

Hash & Dedup: I hash the content (BLAKE3). If the hash exists in the content store, I simply reference the existing segment (deduplication).

Encrypt & Persist: If unique, I encrypt (XTS-AES-256) and append to the NVRAM log. This entire pipeline adds less than 10% overhead compared to a raw write, while potentially saving 2-3x storage space.

5. Operations & Usage
Q: How do I manage SPACE?
A: I provide a CLI tool, spacectl, for all interactions.

Create/Read: spacectl create --file data.txt / spacectl read <uuid>.

Project Views: spacectl project --view nvme --id <uuid>.

Web Interface: I also offer a real-time dashboard for visualizing the mesh topology, gossip traffic, and node health.

Q: Can I run this today?
A: Yes. I include a Docker-based simulation for testing. You can spin up a 3-node mesh with encryption and deduplication enabled in under 90 seconds:

Bash

docker compose -f containerization/docker-compose.yml up -d
This allows you to test S3 uploads, deduplication ratios, and policy configurations locally.

Q: Which operating systems are supported?
A: I currently support **Linux and Windows**. macOS is not supported due to systematic storage backend incompatibilities.

**Why macOS doesn't work:** All of my foundry storage backend tests fail on macOS with data corruption issues - specifically, data written to disk is read back as zeros instead of the expected content. This appears to be related to platform-specific differences in how macOS handles sparse files and direct I/O operations compared to Linux and Windows. The issue affects all core storage operations: the Legacy backend, Magma backend, snapshots, and recovery systems.

**What this means:** While SPACE will compile and run on macOS, the storage layer is fundamentally broken. Any data written would be silently corrupted, making macOS unsuitable for development or production use.

**Future support:** Adding macOS support would require significant platform-specific engineering work to understand and work around macOS's file I/O behavior. This is a known issue tracked in the project, but is not currently prioritized.

**Recommended platforms:**
- **Linux:** Fully supported, primary development platform
- **Windows:** Fully supported, tested in CI
- **macOS:** Not supported - do not use

6. Volumes & Snapshots (Phase 8)
Q: What is the Foundry, and how does it relate to capsules?
A: The Foundry is my high-performance mutable block storage layer (Phase 8). While capsules are immutable content-addressed storage, Foundry provides traditional volumes (like virtual disks) with random read/write access. It bridges two worlds:

Hot Storage: Foundry volumes provide fast, mutable block devices for databases, VMs, and applications.

Cold Storage: The Capsule Registry provides immutable, deduplicated storage for backups and archives.

Together, they form a complete storage solution: work on hot volumes, snapshot to cold capsules.

Q: How do snapshots work in SPACE?
A: My Snapshot Engine (Milestone 8.1: The Bridge) creates point-in-time copies of volumes by:

Chunking: Split the volume into 64KB blocks for optimal deduplication.

Deduplication: Store each block as a capsule. Identical blocks (zeros, OS files) are stored only once globally.

Manifest: Create a JSON manifest capsule that maps volume offsets to capsule IDs.

Atomicity: The snapshot doesn't exist until the manifest is written—it's either complete or doesn't exist.

This design means a 100GB sparse volume with 1MB of actual data only consumes ~1MB of storage.

Q: Can I restore a snapshot to a different volume?
A: Yes. The restore operation is flexible:

Same Volume: Restore over the existing volume (destructive).

Different Volume: Create a new volume and restore into it (auto-resizes).

Different Node: Since manifests are capsules with UUIDs, you can restore on any node in the mesh.

This enables disaster recovery, volume cloning, and test environment creation.

Q: What policies can I apply to snapshots?
A: Snapshots respect the full Policy system:

Default: LZ4 compression + deduplication (fast, space-efficient).

Text-Optimized: Zstd level 3 compression for logs and text data (higher compression ratio).

Encrypted: XTS-AES-256 encryption for sensitive data.

Custom: Any combination of compression, encryption, and deduplication.

The policy is applied during snapshot creation and automatically reversed during restore.

Q: How fast are snapshots?
A: Performance depends on volume size and policy:

Small Volumes (10MB): ~100-200ms for snapshot, ~50-100ms for restore.

Large Volumes (1GB): ~5-10s for snapshot, ~3-7s for restore (throughput ~100-200 MB/s).

Dedup Ratio: 2-10x space savings for OS images and databases with common patterns.

Q: Can I expose Foundry volumes over the network?
A: Yes. Milestone 8.2 implements NVMe-oF (NVMe over Fabrics) binding, allowing any Linux kernel to mount a Foundry volume as a local NVMe block device over TCP/IP. The implementation features:

SPDK Integration: An async bridge between SPDK's polling reactor and Tokio's async runtime.

Lock-Free I/O: MPSC channels for command submission and lock-free queues for completions.

Network Exposure: Volumes are exposed via standard NVMe-oF protocol on TCP port 4420 (configurable).

Kernel Mounting: Linux clients use standard nvme-cli tools to connect: sudo nvme connect -t tcp -n nqn.2024-01.io.space:vol-1 -a 127.0.0.1 -s 4420.

This transforms Foundry from local block storage into network-attached storage while maintaining full NVMe protocol compatibility. Use spacectl expose --volume-id <UUID> --name vol-1 --port 4420 to start exposing a volume.

Q: How does Magma handle crash recovery and durability?
A: My MagmaBackend (Milestone 8.3: The Journal) implements enterprise-grade crash recovery through a checkpoint-and-replay pattern:

Block Headers: Every write includes a 16-byte header with magic bytes ("MGMA"), logical block address, and length for validation.

Checkpoints: I periodically save the complete L2P (logical-to-physical) map and write head position to disk using atomic write-then-rename operations.

Log Replay: On startup after a crash, I load the last checkpoint and replay any writes that occurred after it by scanning block headers until EOF.

Graceful Recovery: If I encounter corrupted headers during replay, I stop gracefully and preserve all valid data written before the corruption.

This design ensures your data survives unexpected power loss, system crashes, or unclean shutdowns. When you call sync() on a Magma volume, I both flush the device AND create a checkpoint, guaranteeing durability. Performance impact is minimal: ~0.4% storage overhead for 4KB blocks, and checkpoint operations typically complete in 50-100ms for 10,000 L2P entries.

Q: Does crash recovery work with the existing Magma volumes?
A: No. Milestone 8.3 introduces a breaking disk format change. Existing Magma volumes created before this milestone cannot be opened with the new code because the old format lacks block headers and checkpoint files. However, since Magma is documented as experimental with no production deployments, this one-time breaking change is acceptable. The migration path is: (1) Before upgrading, snapshot all Magma volumes using the Snapshot Engine. (2) After upgrading, delete old .magma device files. (3) Restore from snapshots. This breaking change enables long-term data integrity validation and supports future features like garbage collection and replication.

Future optimizations include incremental snapshots (only changed blocks) and copy-on-write for instant snapshots.

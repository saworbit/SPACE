# Changelog

All notable changes to the SPACE project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Resilience & Force Snapshot controls** - New `Telemetry::ForcePolicyExecution` enables on-demand RPO execution; ScalingAgent/PolicyCompiler honor forced RPOs; `spacectl snapshot trigger --id <CAPSULE_UUID> [--rpo-secs N] --wait` emits operator commands for deterministic DR testing; resilience harness in `crates/scaling/tests/resilience_test.rs` exercises metro-sync failover and forced snapshot compilation. 
- **Inbound replication test harness** - New `replication_integration` suite spins up real mesh nodes to validate receiver-side persistence, dedup refcounts, MAC tamper rejection, and garbage-frame handling over TCP.
- **Inbound replication hardening** - Mesh replication now pools persistent TCP connections for streaming frames and uses an in-flight reservation registry to guarantee at-most-once NvramLog writes for identical payloads; timeouts guard slowloris cases.
- **Encryption-transparent protocol views** - `RegistryTransformOps` now decrypts/encrypts on-the-fly for Phase 4 view projection, with `scaling::enforce_view_policy` centralizing federation/sharding before NVMe/FUSE/CSI expose plaintext handles; includes a new NVMe integration test that round-trips encrypted storage through the view pipeline.
- **Performance fix spec + benchmark** - Documented the async runtime bridging issue and added a Criterion benchmark (`crates/capsule-registry/benches/runtime_overhead.rs`) to measure per-call runtime creation vs a shared global runtime (`docs/specs/PERFORMANCE_FIX_PIPELINE_RUNTIME.md`).
- **Native NVMe/TCP simulation target** - `crates/sim-nvmeof` now provides a protocol-compliant NVMe/TCP target (no SPDK/hugepages) with helper scripts `scripts/nvmeof_discover.sh` (discover) and `scripts/nvmeof_connect_io.sh` (connect + 4KiB I/O).
- **SPDK-gated NVMe-oF path** - `sim-nvmeof` now uses a `spdk` Cargo feature with Linux-only runtime preflight (hugepages, memlock, root) and automatic fallback to the native TCP target to avoid CI/container hangs.
- **Linux zero-copy replication path** - Outbound replication now uses `tokio-uring` on Linux with a bounded queue, queue-depth logging, and backpressure when saturated; includes `scripts/replication_io_uring_smoke.sh` + `uring_probe` example to validate the io_uring data plane.
- **Raft-ready sled metadata store** - Capsule registry now persists to `space.db` via sled with streaming snapshot/restore and Raft-facing apply hooks, eliminating the JSON SPOF and enabling crash-safe recovery.
- **SwarmBehavior Transformer Pattern** - Dependency-inverted `TransformOps` trait in `common` enables decrypt -> decompress -> re-compress -> re-encrypt during migration with sovereignty enforcement; documented in `docs/specs/PODMS_SWARM_BEHAVIOR.md`.
- **SwarmOps runtime adapter** - `crates/scaling/src/swarm_ops.rs` implements `TransformOps` with per-capsule XTS key derivation and LZ4/Zstd bridging; detailed in `docs/specs/PODMS_TRANSFORM_OPS.md`.
- **BatchQueue byte ceiling** - Hybrid flush trigger now enforces `max_batch_bytes` (default 4MiB helper) to stop oversized payloads from bypassing count-based limits; targeted tests live in `scripts/test_batch_queue_limits.sh`.
- **🌐 Multi-Node Capabilities (PODMS Orchestrator)** - Comprehensive distributed mesh networking
  - **New crate: `podms-orchestrator`** - Unified coordination layer for multi-node operations
    - `Orchestrator` struct wires gossip, mesh, scaling agent, and telemetry channels
    - `OrchestratorConfig` supports YAML/environment configuration
    - `OrchestratorRuntime` provides simplified API for telemetry emission and cluster queries
    - Gossip-to-telemetry bridge translates epidemic broadcasts into autonomous actions
    - Event-driven architecture with mpsc telemetry bus
  - **Enhanced Gossip Layer** - Secure epidemic state propagation
    - HMAC-SHA256 message signing with configurable keys
    - TTL-based flood control (default: 10 hops)
    - Message deduplication via SHA256 message IDs
    - Configurable fanout (default: 8 peers) for bandwidth optimization
    - Timestamp validation for replay attack prevention
  - **Autonomous Scaling** - Policy-driven operations without human intervention
    - Metro-sync replication (zero-RPO, <2ms latency)
    - Async-batch replication (5min RPO, optimized bandwidth)
    - Heat-based migration for hot data redistribution
    - Capacity-driven rebalancing across underutilized nodes
    - Node evacuation (immediate parallel or gradual sequential)
  - **Transformation in Transit** - Secure data migration
    - Re-encryption during migration without decryption exposure
    - Re-compression with different levels (LZ4 → Zstd)
    - Key rotation support during segment transfer
    - BLAKE3 MAC validation on all replicated segments
    - Deterministic encryption preservation for cross-node deduplication
  - **Docker Compose Simulation** - Multi-node development environment
    - 3-node mesh with seed-based discovery
    - Prometheus metrics scraping (15s interval)
    - Grafana dashboards for visualization
    - Isolated network (172.20.0.0/16)
    - Per-node S3 API, Web UI, and replication endpoints
  - **Comprehensive Documentation**
    - [Multi-Node Deployment Guide](docs/multi-node-deployment.md) - 400+ line operations manual
    - [Implementation Summary](docs/MULTI_NODE_IMPLEMENTATION.md) - Complete technical deep-dive
    - Architecture diagrams, configuration examples, troubleshooting guide
    - Performance expectations and security considerations
  - **Integration Test Framework** - Ready for ContentStore implementation
    - Tests for gossip propagation, policy compilation, autonomous replication
    - Migration with transformation, evacuation, rebalancing
    - Cross-node deduplication, message signing, TTL flood control
  - **Sovereignty Enforcement** - Data placement constraints
    - Local (no replication), Zone (metro/geo only), Global (unrestricted)
    - Policy validation before migration/replication

### Changed

- **S3 protocol now streams** - `protocol-s3` handlers and `S3View` accept streaming bodies (Axum `Body::from_stream`), avoiding O(N) buffering for PUT/GET while keeping a temporary bridge to the legacy pipeline; documented in `docs/specs/PERFORMANCE_FIX_S3_STREAMING.md` with new streaming tests.
- **Global async runtime for sync pipeline** - `capsule-registry` uses a single `tokio` runtime (via `OnceLock`) for synchronous bridge calls instead of constructing a runtime per operation, eliminating millisecond-scale latency spikes and adding a warning when called from an async context.
- **WritePipeline runtime strategy** - `capsule-registry` now uses a Strategy-pattern facade: when built with `modular_pipeline`, it prefers the modular orchestrator unless `SPACE_DISABLE_MODULAR_PIPELINE=1` is set, can be forced with `SPACE_USE_MODULAR=1`, and falls back to the legacy path on initialization errors. Legacy telemetry/config methods remain available via downcasting.
    - Compile-time enforcement via compiler checks
- **Native range reads + backfill** - `PipelineStrategy::read_range` is first-class with modular and legacy implementations doing segment-aware reads; segment metadata now records `plain_len` and backfills on read to skip decrypt/decompress for pre-range segments, reducing I/O amplification for existing capsules.

- **Web Interface File Storage** - Complete file management system
  - In-memory file storage with `HashMap<String, StoredFile>`
  - `GET /api/files` endpoint to list all stored files with metadata (size, hash, upload time)
  - `GET /api/files/:path` endpoint for binary file downloads
  - Enhanced `POST /api/upload` to persist files after gossip broadcast
  - Interactive dashboard with "Stored Files" section showing real-time file list
  - One-click download buttons for each stored file
  - Auto-refresh every 5 seconds for live file list updates
  - Fixed JavaScript upload with chunked base64 encoding for large files
  - File operations trigger gossip `FileUploaded` messages to notify peers
  - Comprehensive integration tests for upload, list, and download flows

- **Automation handlers** - Migration, evacuation, and rebalancing now perform real data movement via `ScalingAgent::with_runtime`
  - MAC validation and optional re-encryption during moves
  - Capsule streaming uses replication frames with post-receive deduplication
  - Evacuation parallelism and balanced fan-out over discovered peers
- **Runtime handles** - `capsule_registry::runtime::RuntimeHandles::from_env` wires registry/log/key-manager for production ScalingAgent construction
- **Replication Execution System** - Materialized real replicas from policy actions
  - **Replication-frame mirroring** - `mirror_segment()` wraps segments in `ReplicationFrame`
  - **Metro-sync execution** - Synchronous replication for zero-RPO policies
  - **Async batching queue** - Geo-replication with configurable RPO intervals (default 5 min)
  - **Batch queue implementation** with interval-based and size-based flushing
  - **Outbound sender methods** - `mirror_segment()` (frame-based), `send_replication_frame()`
  - **Agent execution layer** - `execute_metro_sync_replication()` with segment loading
  - **Queue statistics** - Track batch depth, unique capsules, total bytes
  - **Comprehensive documentation** - [docs/replication-actions.md](docs/replication-actions.md) with Mermaid flow diagrams
  - Added `hex` dependency for hash encoding in logs

- **Inbound Replication System** - Complete implementation fixing data discard issue
  - Wire protocol with length-prefixed bincode frames (`ReplicationFrame`)
  - BLAKE3 MAC validation for integrity checking
  - XTS-AES-256 decryption with versioned key management
  - Content-addressable deduplication using BLAKE3 hashing
  - NvramLog persistence with fsync for durability
  - Reference counting for deduplicated segments
  - Generic `ContentStore` trait to avoid circular dependencies
  - Comprehensive error handling and structured logging
  - DoS protection with frame size limits (16MB max)
  - Documentation: [docs/replication.md](docs/replication.md) with Mermaid diagrams

- **Scaling Crate Enhancements**
  - Made `MeshNode` generic over `ContentStore` trait
  - Made `ScalingAgent` generic with `'static` lifetime bounds
  - Added `ReplicationHandler<C: ContentStore>` for inbound processing
  - Exported replication types: `ContentStore`, `ReplicationFrame`, `ReplicationHandler`

- **Dependencies**
  - `bytes` (v1) for zero-copy buffer management
  - `bincode` (v1) for wire protocol serialization
  - `encryption` crate for MAC validation and XTS decryption
  - `nvram-sim` crate for durable log persistence

- **Documentation**
  - Comprehensive replication guide with security guarantees
  - Multi-node setup instructions (Docker Compose + manual)
  - Performance benchmarks and optimization strategies
  - Troubleshooting guide for common issues
  - Implementation status tracking document

### Changed
- Scaling migration/evacuation now invokes `SwarmOps` for decrypt -> decompress -> recompress -> re-encrypt, deriving per-capsule keys and emitting fresh metadata/MACs before streaming replication frames.
- Envelope encryption added: segment payloads use convergent segment keys and store a `wrapped_segment_key` in metadata, wrapped with the per-capsule key to satisfy Zero Trust while preserving deduplication.
- `TransformOps::encrypt/decrypt` now take `capsule_id` so crypto paths can derive per-capsule keys; `Capsule::apply_transform` forwards the capsule id to the runtime ops.
- `MeshNode::new()` now requires `ContentStore`, `NvramLog`, and `KeyManager` dependencies
- `ScalingAgent` now generic over `ContentStore` implementation
- Replication handler integrated into mesh listener spawn logic
- Updated Cargo.toml dependencies for scaling crate
- Updated mirror_segment documentation and call sites to the replication-frame signature
- Added ISC and MPL-2.0 to allowed licenses in deny.toml (both OSI-approved and FSF Free/Libre)

### Fixed
- **Critical:** Inbound replication data discard issue - segments now properly validated, decrypted, deduplicated, and persisted
- **BatchQueue OOM guard:** Async batching now tracks pending bytes and flushes when either count or byte thresholds are reached, preventing memory blowouts from few large items (`docs/specs/PERFORMANCE_FIX_BATCH_QUEUE_OOM.md`).
- Compilation errors in `ScalingAgent` due to missing generic parameters
- Lifetime bound issues with async spawning
- Code quality issues identified by clippy and cargo fmt
  - Removed unused imports in gossip-layer and web-interface
  - Fixed derivable Default implementation in mesh-core NodeRole
  - Fixed needless borrow in gossip-layer message.rs
  - Added #[allow(dead_code)] for intentionally stored fields in GossipImpl
- Unused variable and import warnings

### Security
- **RUSTSEC-2024-0437:** Fixed critical protobuf vulnerability (uncontrolled recursion)
  - Upgraded prometheus from 0.13.3 to 0.14.0
  - Upgraded protobuf from 2.28.0 to 3.7.2 (transitive dependency)
  - Added `.cargo/audit.toml` to ignore unmaintained warnings from transitive dependencies
- Added BLAKE3-based MAC validation to prevent tampering
- Implemented constant-time MAC comparison to prevent timing attacks
- Enforced key version validation via KeyManager
- Added frame size limits to prevent DoS attacks
- Documented security guarantees in replication.md

## [0.2.0] - Phase A DataMotion

### Added
- `DataMotion` unified transport engine in `ScalingAgent`.
- Support for real payload transmission in `execute_metro_sync_replication`.

### Changed
- Refactored `migrate_capsule_task` to be generic over `MotionMode` (Copy vs Move).
- Unified security transformation logic for all data movement types.

## [0.1.0] - 2024-11-15

### Added
- Initial project structure with multi-crate workspace
- Phase 1: Core compression (LZ4, Zstd) and deduplication (BLAKE3)
- Phase 2: Capsule registry with metadata management
- Phase 3: XTS-AES-256 encryption with KeyManager
  - HKDF-based key derivation
  - BLAKE3 MAC for integrity
  - Deterministic tweaks for convergent encryption
- Phase 4: PODMS framework
  - Policy compiler for autonomous scaling
  - Mesh networking stub with manual peer registration
  - ScalingAgent for telemetry-driven operations
- Protocol views: S3, NFS, Block, NVMe-oF (stubs)
- Simulation mode with NVRAM/NVMe-oF mocks
- CLI tool (spacectl) with capsule operations

### Documentation
- README with quick start and architecture overview
- PODMS design document
- Encryption design document
- Simulation mode guide
- Containerization instructions

---

**Note:** This is the first changelog entry. Previous development history can be found in git commit logs.

For detailed implementation notes, see:
- [IMPLEMENTATION_COMPLETE.md](docs/implementation/IMPLEMENTATION_COMPLETE.md) - Inbound replication details
- [INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md](docs/status/INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md) - Progress tracking

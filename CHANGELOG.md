# Changelog

All notable changes to the SPACE project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
- `MeshNode::new()` now requires `ContentStore`, `NvramLog`, and `KeyManager` dependencies
- `ScalingAgent` now generic over `ContentStore` implementation
- Replication handler integrated into mesh listener spawn logic
- Updated Cargo.toml dependencies for scaling crate
- Updated mirror_segment documentation and call sites to the replication-frame signature
- Added ISC and MPL-2.0 to allowed licenses in deny.toml (both OSI-approved and FSF Free/Libre)

### Fixed
- **Critical:** Inbound replication data discard issue - segments now properly validated, decrypted, deduplicated, and persisted
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
- [IMPLEMENTATION_COMPLETE.md](IMPLEMENTATION_COMPLETE.md) - Inbound replication details
- [INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md](INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md) - Progress tracking

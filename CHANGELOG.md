# Changelog

All notable changes to the SPACE project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Replication Execution System** - Materialized real replicas from policy actions
  - **Hash-first dedup mirroring** - Send 32-byte hash before full segment, skip on dedup hit
  - **Metro-sync execution** - Synchronous replication for zero-RPO policies
  - **Async batching queue** - Geo-replication with configurable RPO intervals (default 5 min)
  - **Dedup-preserving protocol** - Wire protocol: hash → response (hit/miss) → optional full data
  - **Batch queue implementation** with interval-based and size-based flushing
  - **Outbound sender methods** - `mirror_segment()` with dedup check, `send_replication_frame()`
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

### Fixed
- **Critical:** Inbound replication data discard issue - segments now properly validated, decrypted, deduplicated, and persisted
- Compilation errors in `ScalingAgent` due to missing generic parameters
- Lifetime bound issues with async spawning
- Unused variable and import warnings

### Security
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

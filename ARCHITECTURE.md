# SPACE Architecture Document

> **Storage Platform for Adaptive Computational Ecosystems**

## Executive Summary

SPACE is a pre-alpha research Rust project implementing a **universal storage fabric** where everything is a **Capsule** (a 128-bit UUID-identified object) that can be accessed through multiple protocol views (S3, NFS, Block/NVMe-oF) simultaneously. The core innovation is enabling encryption, compression, and deduplication to coexist without compromising space efficiency.

**Status:** Pre-alpha, research-grade software. Core storage is Beta quality; multi-node features are experimental proof-of-concepts.

---

## 1. Problem Statement

Traditional storage systems force users to choose **block OR file OR object** storage, duplicating data across protocol silos and creating operational complexity.

**SPACE's Vision:**
- **One universal object** (Capsule) per data unit
- **Multiple simultaneous protocol views** (block, file, object) on the same data
- **Built-in efficiency:** Compression + Deduplication + Encryption coexist
- **Policy-driven autonomy:** Capsules carry their own replication/migration contracts
- **Zero-trust security:** Per-segment encryption with deterministic tweaks preserving dedup

### Core Innovation: Deterministic Encryption + Dedup

Traditional encryption with random IVs destroys deduplication (identical plaintext → different ciphertext). SPACE uses **deterministic XTS-AES-256 tweaks derived from content hashes**, enabling:
- Identical plaintext → Identical ciphertext (dedup works!)
- 0% overhead for encryption on dedup savings
- BLAKE3-MAC integrity verification per segment

---

## 2. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              CLIENTS                                     │
│         spacectl CLI    │    S3 REST API    │    NFS Mount              │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │
┌────────────────────────────────▼────────────────────────────────────────┐
│                         PROTOCOL VIEWS                                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│  │protocol- │  │protocol- │  │protocol- │  │protocol- │  │protocol- │  │
│  │   s3     │  │   nfs    │  │  block   │  │  nvme    │  │  fuse    │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  │
└───────┼─────────────┼─────────────┼─────────────┼─────────────┼────────┘
        │             │             │             │             │
        └─────────────┴──────┬──────┴─────────────┴─────────────┘
                             │
┌────────────────────────────▼────────────────────────────────────────────┐
│                      CAPSULE REGISTRY                                    │
│                   (crates/capsule-registry)                              │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │ WritePipeline: Segment → Compress → Hash → Dedup → Encrypt      │   │
│  │ ReadPipeline:  Decrypt → Decompress → Assemble                  │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │ Sled Metadata: Capsules, Segments, ContentHash→SegmentId        │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└────────────────────────────┬────────────────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────────────────┐
│                       STORAGE LAYER                                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                   │
│  │  nvram-sim   │  │   foundry    │  │   storage    │                   │
│  │ (append log) │  │ (block vols) │  │ (backends)   │                   │
│  └──────────────┘  └──────────────┘  └──────────────┘                   │
└─────────────────────────────────────────────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────────────────┐
│                    DISTRIBUTED LAYER (PODMS)                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐  │
│  │ gossip-layer │  │   scaling    │  │  federation  │  │   podms-    │  │
│  │  (libp2p)    │  │ (replication)│  │   (Raft)     │  │orchestrator │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  └─────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Crate Structure

### Core Data Types & Metadata
| Crate | Purpose |
|-------|---------|
| `common` | Shared types: `CapsuleId`, `SegmentId`, `Policy`, traits |
| `encryption` | XTS-AES-256 encryption, key management |
| `nvram-sim` | Append-only segment log (persistent storage) |

### Storage Pipeline
| Crate | Purpose |
|-------|---------|
| `capsule-registry` | Core registry with Sled-backed metadata + pipelines |
| `pipeline` | Modular write pipeline trait definitions |
| `compression` | LZ4/Zstd with entropy-based selection |
| `dedup` | BLAKE3 content-addressed deduplication |
| `storage` | Storage backends (in-memory, NVRAM, filesystem) |

### Block Storage (Phase 8: The Foundry)
| Crate | Purpose |
|-------|---------|
| `foundry` | Polymorphic block volumes (Legacy + Magma backends) |
| `tiering` | Hot/cold tiering with heatmap tracking |

### Protocol Views
| Crate | Purpose |
|-------|---------|
| `protocol-s3` | S3-compatible REST API |
| `protocol-nfs` | NFS namespace export |
| `protocol-block` | Block volume facade with COW |
| `protocol-nvme` | NVMe-oF target (SPDK bridge) |
| `protocol-fuse` | FUSE filesystem mount (Unix) |
| `protocol-csi` | Kubernetes CSI driver helpers |

### Distributed/Multi-Node (PODMS)
| Crate | Purpose |
|-------|---------|
| `scaling` | Metro-sync replication, policy compiler |
| `federation` | gRPC WAN bridge, Raft consensus |
| `mesh-core` | Core types: `Peer`, `NodeRole`, `GossipMessage` |
| `gossip-layer` | libp2p-based epidemic gossip |
| `podms-orchestrator` | Multi-node coordination layer |
| `web-interface` | Axum + Leptos web UI |

### Compute & Transform
| Crate | Purpose |
|-------|---------|
| `transform-engine` | Wasmtime WASM sandbox for on-read transforms |
| `layout-engine` | CapsuleFlow layout optimization |

### CLI & Simulation
| Crate | Purpose |
|-------|---------|
| `spacectl` | Command-line interface |
| `sim-nvram` | NVRAM simulation wrapper |
| `sim-nvmeof` | NVMe-oF fabric simulation |
| `xtask` | Custom Cargo build tasks |

---

## 4. Core Data Types

### Primary Entities

```rust
// 128-bit universal object ID
pub struct CapsuleId(pub Uuid);

// 64-bit segment reference
pub struct SegmentId(pub u64);

// Content hash for deduplication
pub struct ContentHash(pub String); // BLAKE3 hex

// Core capsule metadata
pub struct Capsule {
    pub id: CapsuleId,
    pub size: u64,
    pub segments: Vec<SegmentId>,
    pub created_at: u64,
    pub policy: Policy,
    pub deduped_bytes: u64,
}

// Per-segment metadata
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,                 // Offset in NVRAM log
    pub len: u32,
    pub plain_len: Option<u32>,      // Uncompressed size
    pub compressed: bool,
    pub compression_algo: String,
    pub content_hash: Option<ContentHash>,
    pub ref_count: u32,
    pub deduplicated: bool,
    // Encryption fields
    pub encryption_version: Option<u16>,
    pub key_version: Option<u32>,
    pub tweak_nonce: Option<[u8; 16]>,
    pub integrity_tag: Option<[u8; 16]>,
    pub encrypted: bool,
}
```

### Policy System

```rust
pub struct Policy {
    pub compression: CompressionPolicy,  // LZ4/Zstd/None
    pub encryption: EncryptionPolicy,    // XtsAes256/Disabled
    pub federation: FederationPolicy,    // Replication targets
    pub transform: Option<TransformDef>, // WASM transforms
    pub rpo: Duration,                   // Recovery Point Objective
    pub latency_target: Duration,
    pub sovereignty: SovereigntyLevel,   // Local/Zone/Global
}
```

---

## 5. Data Flow

### Write Path

```
┌─────────────────────────────────────────────────────────────┐
│ CLIENT: write_capsule(data, policy)                         │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. SEGMENTATION                                              │
│    Split into 4MB chunks                                     │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. COMPRESSION (Lz4ZstdCompressor)                          │
│    - Entropy detection (skip if >7.5 bits/byte)             │
│    - Policy-driven: LZ4 for hot, Zstd for cold              │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. HASHING (BLAKE3)                                          │
│    - Hash compressed data → ContentHash                     │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. DEDUPLICATION CHECK                                       │
│    - Check if ContentHash exists                             │
│    - HIT: reuse existing SegmentId                          │
│    - MISS: assign new SegmentId, continue to encryption     │
└─────────────────────────────┬───────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              │ MISS                      HIT │
              ▼                               │
┌─────────────────────────────────────────┐   │
│ 5. ENCRYPTION (XTS-AES-256)             │   │
│    - Derive deterministic tweak from    │   │
│      ContentHash (preserves dedup!)     │   │
│    - Encrypt segment                    │   │
│    - Compute BLAKE3-MAC integrity tag   │   │
└─────────────────────────────┬───────────┘   │
                              │               │
                              ▼               │
┌─────────────────────────────────────────┐   │
│ 6. STORAGE                              │   │
│    - Append to NVRAM log                │   │
│    - fsync for durability               │   │
└─────────────────────────────┬───────────┘   │
                              │               │
              ┌───────────────┴───────────────┘
              │
              ▼
┌─────────────────────────────────────────────────────────────┐
│ 7. METADATA UPDATE                                           │
│    - Store Capsule record in Sled                            │
│    - Update ContentHash → SegmentId mapping                 │
│    - Increment ref_count if deduped                         │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
                        Return CapsuleId
```

### Read Path

```
┌─────────────────────────────────────────────────────────────┐
│ CLIENT: read_capsule(capsule_id)                            │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. METADATA LOOKUP                                           │
│    - Get Capsule from Sled registry                          │
│    - Retrieve segment list                                   │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
         ┌────────────────────┴────────────────────┐
         │ For each segment:                       │
         ▼                                         │
┌─────────────────────────────────────────────┐    │
│ 2. READ FROM STORAGE                        │    │
│    - Read encrypted bytes from NVRAM        │    │
└─────────────────────────────┬───────────────┘    │
                              │                    │
                              ▼                    │
┌─────────────────────────────────────────────┐    │
│ 3. DECRYPT                                  │    │
│    - Verify MAC tag (integrity check)       │    │
│    - Derive same deterministic tweak        │    │
│    - XTS-AES-256 decrypt                    │    │
└─────────────────────────────┬───────────────┘    │
                              │                    │
                              ▼                    │
┌─────────────────────────────────────────────┐    │
│ 4. DECOMPRESS                               │    │
│    - Apply inverse of compression_algo      │    │
└─────────────────────────────┬───────────────┘    │
                              │                    │
         ┌────────────────────┴────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. ASSEMBLE                                                  │
│    - Concatenate all segment plaintexts                      │
│    - Return to client                                        │
└─────────────────────────────────────────────────────────────┘
```

---

## 6. Key Subsystems

### 6.1 Storage Layer

**StorageBackend Trait:**
```rust
pub trait StorageBackend: Send + Sync {
    fn append(&mut self, segment: SegmentId, data: &[u8]) -> BoxFuture<Result<()>>;
    fn read(&self, segment: SegmentId) -> BoxFuture<Result<Vec<u8>>>;
    fn metadata(&self, segment: SegmentId) -> BoxFuture<Result<Segment>>;
    fn delete(&mut self, segment: SegmentId) -> BoxFuture<Result<()>>;
}
```

**Implementations:**
- `InMemoryBackend` - Testing (HashMap-based)
- `NvramBackend` - Production (file-backed append-only log)
- `UringBackend` - Linux io_uring for zero-copy I/O

### 6.2 Compression

**Lz4ZstdCompressor:**
- Entropy detection (skip if >7.5 bits/byte)
- Adaptive: LZ4 for speed, Zstd for ratio
- Zero-copy path with `Cow<[u8]>`

### 6.3 Deduplication

**Blake3Deduper:**
- BLAKE3 hash of compressed data
- In-memory index: `ContentHash → SegmentId`
- Reference counting per segment

### 6.4 Encryption

**XtsEncryptor:**
1. Derive deterministic tweak from ContentHash
2. XTS-AES-256 encrypt (512-bit key total)
3. Compute BLAKE3-keyed MAC
4. Store metadata: key_version, tweak, integrity_tag

**Key Property:** Same plaintext → Same ciphertext (dedup preserved!)

### 6.5 Foundry (Phase 8 Block Storage)

```rust
pub trait VolumeBackend: Send + Sync {
    async fn write_at(&self, offset: u64, data: Bytes) -> Result<()>;
    async fn read_at(&self, offset: u64, len: usize) -> Result<Bytes>;
    async fn sync(&self) -> Result<()>;
}
```

**Backends:**
- `LegacyBackend` - File-based sparse volumes
- `MagmaBackend` - Log-structured with L2P mapping

---

## 7. Distributed Architecture (PODMS)

### Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                     PODMS ORCHESTRATOR                               │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ Gossip Layer (libp2p)                                        │    │
│  │ - Peer discovery via gossipsub                               │    │
│  │ - Heartbeat-based liveness (1s interval)                     │    │
│  └─────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ Policy Compiler                                              │    │
│  │ - Telemetry events → ScalingAction                          │    │
│  │ - RPO evaluation (sync vs async replication)                 │    │
│  └─────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ Scaling Agent                                                │    │
│  │ - Execute: Replicate, Migrate, Evacuate, Rebalance          │    │
│  │ - Metro-sync with MAC validation                             │    │
│  └─────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ Federation Bridge (Raft)                                     │    │
│  │ - Distributed consensus for metadata                         │    │
│  │ - gRPC transport for cross-node communication                │    │
│  └─────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
```

### Replication Protocol

1. **Telemetry** emits `NewCapsule` event
2. **PolicyCompiler** evaluates RPO:
   - RPO=0 → Synchronous metro-sync
   - RPO>0 → Async batch replication
3. **ScalingAgent** creates `ReplicationFrame`
4. **MeshNode** sends to target peers
5. **Receiver** validates MAC, stores replica

---

## 8. Protocol Views

| Protocol | Mapping | Use Case |
|----------|---------|----------|
| **S3** | bucket/key → CapsuleId | Object storage, backup |
| **NFS** | path → CapsuleId | File sharing, legacy apps |
| **Block** | LUN/offset → Capsule sectors | VMs, databases |
| **NVMe-oF** | Namespace → Foundry volume | High-performance block |
| **FUSE** | mountpoint → Capsule namespace | Local filesystem view |
| **CSI** | PVC → Volume | Kubernetes integration |

---

## 9. Key Traits & Patterns

```rust
// Compression
pub trait Compressor: Send + Sync {
    fn compress(&self, data: &[u8], policy: &CompressionPolicy)
        -> Result<(Cow<[u8]>, CompressionSummary)>;
    fn decompress(&self, data: &[u8], algorithm: &str) -> Result<Vec<u8>>;
}

// Deduplication
pub trait Deduper: Send + Sync {
    fn hash_content(&self, data: &[u8]) -> ContentHash;
    fn check_dedup(&self, hash: &ContentHash) -> Option<SegmentId>;
    fn register_content(&mut self, hash: ContentHash, segment: SegmentId);
}

// Encryption
pub trait Encryptor: Send + Sync {
    fn encrypt(&self, data: Cow<[u8]>, policy: &EncryptionPolicy,
               segment: SegmentId) -> Result<(Vec<u8>, EncryptionSummary)>;
    fn decrypt(&self, data: &[u8], policy: &EncryptionPolicy,
               segment: SegmentId) -> Result<Vec<u8>>;
    fn verify_mac(&self, data: &[u8], mac: &[u8], segment: SegmentId) -> Result<()>;
}

// Storage
pub trait StorageBackend: Send + Sync {
    fn append(&mut self, segment: SegmentId, data: &[u8]) -> BoxFuture<Result<()>>;
    fn read(&self, segment: SegmentId) -> BoxFuture<Result<Vec<u8>>>;
}

// Key management
pub trait Keyring: Send + Sync {
    fn get_key(&self, version: u32) -> Result<[u8; 32]>;
}
```

### Design Patterns Used
- **Strategy Pattern** - Pluggable pipeline backends
- **Trait Objects** - Dynamic dispatch for storage/encryption
- **Cow (Clone-on-Write)** - Zero-copy compression
- **Arc<Mutex<T>>** - Shared mutable state
- **Builder Pattern** - Pipeline construction

---

## 10. Configuration

### Environment Variables

```bash
# Storage
SPACE_METADATA_PATH=./space.db          # Sled registry location
SPACE_NVRAM_PATH=./space.nvram          # Append-only log location

# Encryption
SPACE_MASTER_KEY=<64-hex-chars>         # 256-bit master key
SPACE_MASTER_KEY_FILE=/path/to/keyfile  # Alternative: file-based key

# Mesh/PODMS
GOSSIP_FANOUT=20                        # Peer selection fanout
GOSSIP_HEARTBEAT_INTERVAL=1s            # Liveness interval
SPACE_PODMS_RPO_THRESHOLD=60s           # Sync→async cutoff

# Logging
RUST_LOG=info,space=debug
```

---

## 11. Feature Flags

| Feature | Description |
|---------|-------------|
| `pipeline_async` | Async write pipeline |
| `modular_pipeline` | Trait-based orchestrator |
| `advanced-security` | Bloom filters, audit logs, SPIFFE |
| `podms` | Multi-node gossip, scaling, replication |
| `phase4` | Federation gRPC, FUSE |
| `phase5` | WASM transforms |
| `magma` | Log-structured storage backend |
| `rdma` | RDMA transport (Linux) |
| `uring` | tokio-uring I/O (Linux) |

---

## 12. Performance Characteristics

### Compression
| Data Type | Algorithm | Ratio | Throughput |
|-----------|-----------|-------|------------|
| Text/Logs | Zstd-3 | 3-5x | ~500 MB/s |
| Binary | LZ4-1 | 1.5-2.5x | ~2 GB/s |
| Random | Skip | 1.0x | ~5 GB/s |

### Encryption Overhead
- Encrypt: ~5% overhead
- Decrypt: ~9% overhead
- Dedup with encryption: **0% additional overhead**

### Deduplication Ratios
| Workload | Typical Ratio |
|----------|---------------|
| VM Images | 10-20x |
| Logs | 2-5x |
| User Data | 1.5-3x |

---

## 13. Security Model

### Cryptographic Primitives
- **XTS-AES-256** - NIST-standardized disk encryption
- **BLAKE3** - Fast cryptographic hashing
- **Poly1305** - Per-segment MAC verification
- **Kyber** - Post-quantum hybrid (experimental)

### Key Management
- Master key from environment (256 bits)
- Versioned key derivation for rotation
- Zeroization on drop (secure memory)
- BLAKE3-KDF for derived keys

### Zero-Trust (Experimental)
- SPIFFE identity verification
- mTLS transport security
- eBPF ingress policy enforcement

---

## 14. Current Limitations

### Not Production-Ready
- Error recovery needs hardening
- Limited observability/monitoring
- Multi-node stability untested at scale
- No backup/restore tools

### Experimental Features
- Gossip protocol (basic)
- Metro-sync replication (TCP POC)
- FUSE mounting (read-only, Unix)
- WASM transforms (initial)

---

## 15. Roadmap

### Completed
- [x] Phase 1-3: Core storage, compression, dedup, encryption
- [x] Phase 8: Foundry block storage
- [x] Phase 9.1-9.2: Raft consensus foundation

### In Progress
- [ ] Phase 9.3+: Full mesh federation
- [ ] ML-driven placement engine
- [ ] Production hardening

### Future
- [ ] Hardware offload (DPU/GPU)
- [ ] Confidential compute enclaves
- [ ] Full Kubernetes integration

---

## References

- [README.md](README.md) - Project overview and quick start
- [docs/](docs/) - Detailed specifications
- [CHANGELOG.md](CHANGELOG.md) - Version history

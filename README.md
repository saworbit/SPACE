<div align="center">

# 🚀 SPACE
### Storage Platform for Adaptive Computational Ecosystems

[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![CI](https://github.com/saworbit/SPACE/actions/workflows/pr-checks.yml/badge.svg)](https://github.com/saworbit/SPACE/actions/workflows/pr-checks.yml)

### *One capsule. Infinite views.*
**The future of storage starts with a single primitive that breaks down protocol silos.**

---

> ## ⚠️ PRE-ALPHA SOFTWARE
>
> **SPACE is in active early development and NOT production-ready.**
>
> - **Status:** Research prototype & proof-of-concept
> - **Stability:** APIs will change without notice
> - **Use Case:** Educational, research, and experimentation only
> - **Data Safety:** Do not use for production data - expect bugs and breaking changes
> - **Testing:** Single-developer project - limited real-world validation
>
> See the [Feature Status Table](#-feature--capability-status) below for detailed maturity levels.

---

**🔬 Current Focus:** Core storage primitives + distributed consensus (Phase 9)

**✅ Working (Beta/Alpha):** Basic capsule storage • Compression • Deduplication • Encryption • S3 API • Raft consensus • Global registry • Self-driving reconciliation

**🧪 Experimental:** Multi-node mesh • Replication • Protocol views • Federation

[🚀 Quick Start](#-quick-start) • [📚 Documentation](#-documentation) • [🎬 Demo](#-quick-demo) • [📊 Feature Status](#-feature--capability-status)

</div>

---

## 📖 Table of Contents

- [📊 Feature & Capability Status](#-feature--capability-status) **← Start Here**
- [💡 The Big Idea](#-the-big-idea)
- [📊 What Works Today](#-what-works-today)
- [🌐 PODMS Scaling](#-podms-scaling)
- [✨ Development Phases](#-development-phases)
- [🚀 Quick Start](#-quick-start)
- [🏗️ Architecture](#️-architecture)
- [📁 Project Structure](#-project-structure)
- [🧪 Testing](#-testing)
- [🚀 Control Plane API](#-control-plane-api)
- [💡 Why This Matters](#-why-this-matters)
- [🔐 Security & Encryption](#-security--encryption)
- [🗺️ Roadmap](#️-roadmap)
- [⚡ Performance](#-performance)
- [🤝 Contributing](#-contributing)
- [📚 Documentation](#-documentation)
- [📜 License](#-license)
- [📊 Project Status](#-project-status)
- [🎬 Quick Demo](#-quick-demo)

---

## 📊 Feature & Capability Status

> **Important:** This table reflects the *actual* implementation status vs. documented capabilities. SPACE is a pre-alpha research project with many features in experimental or proof-of-concept stages.

### Status Definitions

| Status | Meaning | Use In Production? |
|:------:|:--------|:------------------:|
| **🟢 Beta** | Core functionality works, tested, some bugs expected | ⚠️ Not yet |
| **🟡 Alpha** | Basic implementation, limited testing, expect issues | ❌ No |
| **🟠 Experimental** | Proof-of-concept, incomplete, unstable | ❌ No |
| **🔴 Planned** | Design/docs exist, minimal or no implementation | ❌ No |
| **⚪ Stub** | Placeholder code only, not functional | ❌ No |

### Core Storage Features

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **Capsule Storage** | 🟢 Beta | Basic create/read operations work | - |
| **Metadata Registry** | 🟢 Beta | Sled-backed persistence, tested | - |
| **NVRAM Log Simulator** | 🟢 Beta | File-backed append-only log | - |
| **4MB Segmentation** | 🟢 Beta | Automatic chunking works | - |
| **CLI Tools (spacectl)** | 🟡 Alpha | Basic commands work, UX rough | - |
| **Write Pipeline** | 🟢 Beta | Sync pipeline solid, async experimental | `pipeline_async` |
| **Read Pipeline** | 🟢 Beta | Decompression/decryption works | - |
| **Error Handling** | 🟡 Alpha | Mutex recovery, descriptive panics, path validation | - |

### Compression & Deduplication

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **LZ4 Compression** | 🟢 Beta | Fast compression works well | - |
| **Zstd Compression** | 🟢 Beta | High-ratio compression works | - |
| **Entropy Detection** | 🟡 Alpha | Skips incompressible data | - |
| **Policy-Driven Compression** | 🟡 Alpha | Per-capsule policies work | - |
| **Content Deduplication** | 🟢 Beta | BLAKE3 hashing, post-compression | - |
| **Dedup Statistics** | 🟡 Alpha | Basic tracking implemented | - |
| **Reference Counting** | 🟡 Alpha | Tracks segment usage | - |
| **Garbage Collection** | 🟡 Alpha | Manual GC only, no auto-reclaim | - |

### Encryption & Security

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **XTS-AES-256 Encryption** | 🟢 Beta | Per-segment encryption works | - |
| **BLAKE3-MAC Integrity** | 🟢 Beta | Tamper detection implemented | - |
| **Deterministic Encryption** | 🟡 Alpha | Preserves dedup, needs more testing | - |
| **Key Management** | 🟡 Alpha | Basic derivation/rotation works | - |
| **Key Rotation** | 🟡 Alpha | Version tracking, limited testing | - |
| **Counting Bloom Filters** | 🟡 Alpha | Registry screening works | `advanced-security` |
| **Audit Log** | 🟡 Alpha | BLAKE3 chaining, TSA hooks stubbed | `advanced-security` |
| **SPIFFE/mTLS Gateway** | 🟠 Experimental | eBPF hooks with poison-safe RwLock | `advanced-security` |
| **Post-Quantum Crypto** | 🟠 Experimental | Kyber hybrid toggle, untested | `advanced-security` |

### Block Storage (Phase 8: The Foundry)

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **VolumeBackend Abstraction** | 🟢 Beta | Block-level volume trait with BoxFuture pattern | - |
| **LegacyBackend (File-based)** | 🟢 Beta | Sparse file volumes, works on all platforms | - |
| **MagmaBackend (Log-structured)** | 🟢 Beta | L2P mapping, append-only writes, crash recovery | `magma` |
| **Magma Durability** | 🟢 Beta | Checkpoint + log replay recovery (Milestone 8.3) | - |
| **DirectIoDevice** | ⚪ Stub | Abstraction for SPDK/raw device (tokio::fs for now) | - |
| **Volume Management** | 🟢 Beta | Create, get, delete, list volumes | - |
| **Concurrent Access** | 🟡 Alpha | Thread-safe reads, sequential writes recommended | - |
| **Sparse Volumes** | 🟢 Beta | Filesystem-backed sparse file support | - |
| **Online Resize** | 🟢 Beta | LegacyBackend supports volume resize | - |
| **Bounds Checking** | 🟢 Beta | Automatic validation of read/write offsets | - |
| **Windows Compatibility** | 🟢 Beta | File sharing, sparse file support | - |
| **Snapshot Engine** | 🟢 Beta | Point-in-time volume snapshots to capsules (Milestone 8.1) | - |
| **Snapshot Restore** | 🟢 Beta | Restore snapshots to same or different volume | - |
| **Policy-Aware Snapshots** | 🟢 Beta | Compression, encryption, deduplication support | - |
| **Sparse Snapshot Optimization** | 🟢 Beta | 64KB chunking with global zero-block dedup | - |
| **Chain Replication** | 🟢 Beta | Synchronous replication for zero RPO (Milestone 8.4) | - |

### Protocol Views

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **S3 API** | 🟡 Alpha | Basic PUT/GET/DELETE work, incomplete | - |
| **S3 Streaming** | 🟡 Alpha | Upload/download without full buffering | - |
| **S3 Multipart** | 🔴 Planned | Not implemented | - |
| **NFS Export** | 🟠 Experimental | Basic namespace, minimal testing | - |
| **Block Volumes** | 🟠 Experimental | LUN facade with COW, prototype only | - |
| **NVMe-oF Target (Foundry)** | 🟡 Alpha | SPDK async bridge for Foundry volumes | - |
| **NVMe-oF Target (Capsule)** | 🟠 Experimental | SPDK-backed capsule projection (simulated) | `phase4` |
| **FUSE Filesystem** | 🟠 Experimental | Local capsule projection via `content` view | `phase4` |
| **CSI Driver (K8s)** | 🟠 Experimental | Provision/publish helpers (not a full CSI deployment yet) | `phase4` |

### Multi-Node & Distributed Features

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **PODMS Scaling** | 🟠 Experimental | Types/telemetry exist, limited integration | `podms` |
| **Mesh Networking** | 🟠 Experimental | Basic peer discovery works | `podms` |
| **Gossip Protocol** | 🟠 Experimental | libp2p-based, early stage | `podms` |
| **Metro-Sync Replication** | 🟠 Experimental | TCP-based POC, RDMA mocked, 16 MiB frame limit, TLS-ready | `podms` |
| **Async Replication** | 🟠 Experimental | Batch queue exists, needs testing | `podms` |
| **Policy Compiler** | 🟡 Alpha | Telemetry → actions works | `podms` |
| **Scaling Agents** | 🟠 Experimental | Basic agent loop, minimal coverage | `podms` |
| **Cross-Node Dedup** | 🟠 Experimental | Hash-based preservation attempted | `podms` |
| **Transformation in Transit** | 🟠 Experimental | Re-encrypt/compress design exists | `podms` |
| **Raft Consensus (metadata)** | 🟠 Experimental | Capsule metadata replicated via Raft (openraft + gRPC); `spacectl server start --bootstrap/--join` | - |
| **Raft Control Plane (Phase 9.1)** | 🟡 Alpha | tikv/raft-rs consensus engine with async tokio integration | - |
| **Persistent Raft Storage (Phase 9.2)** | 🟡 Alpha | SledStorage for durability, gRPC transport for multi-node clusters | - |
| **Global Registry (Phase 9.3)** | 🟡 Alpha | Deterministic state machine for cluster topology (nodes, volumes, replicas) | - |
| **Node Reconciliation (Phase 9.4)** | 🟡 Alpha | Level-triggered control loop converges local Foundry state to match global Registry; self-driving volume creation/deletion | - |
| **Federated Metadata Sharding (Phase 4)** | 🟠 Experimental | Policy-driven federation/sharding hooks + Phase 4b gRPC bridge | `phase4` |

### Monitoring & Operations

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **Web Interface** | 🟠 Experimental | Basic dashboard, limited features | - |
| **Prometheus Metrics** | 🟡 Alpha | Basic metrics exposed | - |
| **WebSocket Updates** | 🟠 Experimental | Live topology updates prototype | - |
| **Tracing/Logging** | 🟡 Alpha | Basic tracing implemented | - |
| **Health Checks** | 🟡 Alpha | Basic health endpoints | - |

### Simulation & Testing

| Feature | Status | Notes | Build Flag |
|:--------|:------:|:------|:-----------|
| **NVRAM Simulation** | 🟢 Beta | File-backed testing works | - |
| **NVMe-oF Simulation** | 🟡 Alpha | Native NVMe/TCP with fallback | - |
| **Docker Compose Setup** | 🟡 Alpha | 3-node mesh environment | - |
| **Integration Tests** | 🟡 Alpha | Basic coverage, needs expansion | - |
| **Unit Tests** | 🟡 Alpha | ~70-80% coverage, gaps exist | - |
| **Benchmarks** | 🟠 Experimental | Limited performance tests | - |

### Key Gaps & Limitations

**❌ Missing or Incomplete:**
- Production-grade error recovery
- Comprehensive logging and observability
- Performance optimization and benchmarking
- Full penetration testing and third-party security audit
- Multi-node stability and failover
- Data migration tools
- Backup and restore
- Monitoring and alerting
- Documentation completeness
- Real-world validation

**⚠️ Known Issues:**
- Single-developer project with limited testing
- Many features are proofs-of-concept
- APIs will change without notice
- Performance not optimized
- Security features need third-party auditing
- Multi-node features are experimental
- Vendor stubs are placeholders

**✅ Recently Hardened:**
- Path traversal protection on all object API endpoints
- Hardcoded dev secrets eliminated (random ephemeral keys in debug builds)
- Deserialization size limits on replication frames (16 MiB cap)
- Mutex poisoning recovery across 6 crates (graceful degradation)
- TLS configuration infrastructure for inter-node transport
- Lock contention reduction in replication crypto path

---

## 💡 The Big Idea

Traditional storage forces you into boxes: **block** *or* **file** *or* **object**.
Different APIs. Separate data copies. Endless complexity.

### SPACE flips the script 🎯

Everything is a **capsule** — a universal 128-bit ID that can be viewed through *any* protocol:

<div align="center">

| Protocol | Access Method | Status |
|:--------:|:-------------:|:------:|
| 🔲 **Block** | NVMe-oF, iSCSI | ✅ Ready |
| 📁 **File** | NFS, SMB | ✅ Ready |
| 🗄️ **Object** | S3 API | ✅ Ready |

</div>

### ✨ One capsule. Three views. Zero copies.

---

## 📊 What Works Today

<div align="center">

**🧪 Pre-Alpha Status — Core Storage Beta, Distributed Features Experimental**

**📊 [See Detailed Feature Status Table Above](#-feature--capability-status) for maturity levels**

</div>

### Core Features (Beta Quality - Relatively Stable)
- Universal capsule storage with persistent metadata (Sled-backed registry)
- CLI create/read operations via spacectl (basic functionality works)
- Adaptive compression (LZ4/Zstd with entropy detection) - working well
- Content-addressed deduplication (post-compression, BLAKE3 hashing) - functional
- Zero-copy streaming reads (
ead_capsule_stream) with constant-memory Bytes per segment
- Cursor-based registry listing (list_capsules(limit, cursor)) to page infinite capsule counts
- Async-only pipeline and CLI (no block_on bridges) to maximize concurrency
- XTS-AES-256 encryption with BLAKE3-MAC integrity - basic implementation works
- Deterministic encryption preserving deduplication - needs more testing
- NVRAM log simulator for persistent segment storage

### 🟡 Alpha Features (Working But Rough)
- 🌐 **S3-compatible REST API** - Basic PUT/GET/DELETE work, incomplete feature set
- **Streaming S3 uploads/downloads** - Reduces memory buffering
- 🔑 **Key management** with rotation support - basic implementation, limited testing
- 🗑️ **Reference-counted GC** with metadata reclamation - manual GC only
- ⚙️ **Async write pipeline** (feature `pipeline_async`) - experimental
- 🧩 **Modular pipeline** (feature `modular_pipeline`) - trait-based design, early stage
- **Policy-driven compression** - per-capsule configuration works

### 🟠 Experimental Features (Proof-of-Concept, Unstable)
- 📂 **NFS + block protocol views** - namespace + volume facades, minimal testing
- 🌸 **Counting Bloom filters** - dedup candidate screening, needs validation
- 📝 **Immutable audit log** - BLAKE3 hash chaining implemented, TSA hooks stubbed
- 🛡️ **SPIFFE + mTLS eBPF gateway** (feature `advanced-security`) - basic hooks only
- 🔮 **Post-quantum crypto toggle** - Kyber hybrid selectable, untested
- 🤝 **PODMS mesh/replication** - types and telemetry exist, integration incomplete
- **Automation handlers** for migration/evacuation - design exists, limited implementation

### ⚠️ Important Caveats
- Most "complete" features are **beta quality at best** - expect bugs
- **Multi-node and distributed features are experimental** - not production-ready
- **Security features need professional audit** before any serious use
- **Performance not optimized** - no comprehensive benchmarks
- **Error handling is basic** - edge cases may not be covered
- **Documentation often describes aspirational goals** rather than current state

### 🧪 Multi-Node Capabilities (EXPERIMENTAL - NOT PRODUCTION READY)

> **⚠️ WARNING:** These features are early-stage proofs-of-concept. Do not use in production.

**PODMS Orchestrator** - Experimental distributed mesh networking (feature flag: `podms`):

- **🟠 Experimental Autonomous Operations** (Proof-of-Concept)
  - Metro-sync replication - TCP-based POC, RDMA mocked
  - Async-batch replication - basic batch queue exists
  - Heat-based migration - design exists, minimal testing
  - Capacity-driven rebalancing - types defined, limited implementation
  - Node evacuation - framework in place, needs validation
  - Policy compiler - telemetry → actions works, needs real-world testing

- **🟠 Experimental Gossip Protocol** (Early Stage)
  - libp2p-based peer discovery - basic implementation
  - Message signing/deduplication - implemented but not battle-tested
  - Flood control - basic TTL mechanism
  - Configurable fanout - parameter exists, tuning needed

- **🟠 Experimental Transformation in Transit** (Design Exists)
  - Re-encryption during migration - `TransformOps` trait defined
  - Re-compression optimization - framework in place
  - Key rotation support - basic version tracking
  - BLAKE3 MAC validation - implemented in pipeline
  - Cross-node deduplication - hash-based preservation attempted
  - SwarmOps adapter - dependency injection working, needs extensive testing

- **🟡 Docker Compose Simulation** (Alpha Quality)
  - 3-node mesh environment - basic setup works
  - Prometheus + Grafana - metrics exposed, dashboards basic
  - Isolated network testing - functional for development

**Reality Check:**
- Multi-node features are **research prototypes**
- **Not tested at scale** - single developer, limited validation
- **Many edge cases unhandled** - error recovery incomplete
- **Performance unoptimized** - no production benchmarks
- **Failover/resilience not validated** - chaos testing minimal

📘 **[Multi-Node Deployment Guide →](docs/multi-node-deployment.md)** *(describes experimental setup)*

### 🔜 Planned (Not Implemented)
- **Full mesh federation** - design docs exist, Raft stubs only
- **Cross-zone routing** - architecture planned, not built
- **ML-driven heatmaps** - aspirational feature
- **Adaptive placement** - concept stage

---

## 🌐 Web Interface (EXPERIMENTAL)

> **⚠️ Experimental Feature:** Basic dashboard for development/testing only

**Status:** 🟠 Experimental proof-of-concept for mesh visualization

SPACE includes an early-stage web interface for visualizing mesh topology and basic file operations. This is a development tool, not a production admin interface.

#### Control Plane API (RFC-001)
- Versioned, typed routes under `/api/v1` with Swagger UI at `/swagger-ui` and OpenAPI JSON at `/api-docs/openapi.json`.
- JWT guard with RBAC roles (`admin`, `editor`, `viewer`); `system/health` remains public for probes.
- Standard envelope (`success/data/error/meta`) plus pagination helpers (`page`, `limit`, `sort`, `after_id`) on list endpoints.
- Streaming multipart uploads (`POST /api/v1/data/objects`) replace legacy base64 JSON bodies.

### ✨ Features (Experimental)

- 📊 **Peer discovery visualization** - basic gossip protocol display
- 🔄 **WebSocket updates** - live topology changes (early implementation)
- 📈 **Prometheus metrics** - basic metric export
- 🗂️ **File operations** - simple upload/download with Blake3 verification
- 💾 **Storage dashboard** - basic file listing and metadata display
- 🎯 **RBAC framework** - types defined, enforcement incomplete
- ⚡ **Leptos frontend** - reactive UI framework (optional, feature-gated)

**Limitations:**
- Security partially hardened (path traversal fixed, dev secrets removed) but not audited - development use only
- Limited error handling and validation
- UX rough and incomplete
- Not tested with real traffic loads

### 🚀 Quick Start

```bash
# Build and run the web server
cargo run -p web-interface --bin web-server

# Access the dashboard
open http://localhost:3000

# With custom configuration
BIND_ADDR=0.0.0.0:8080 GOSSIP_FANOUT=12 cargo run -p web-interface --bin web-server
```

### 📚 API Endpoints

- `GET /health` - Health check
- `GET /api/peers` - List all peers with gossip metrics
- `GET /api/gossip/stats` - Gossip protocol statistics
- `POST /api/upload` - Upload file to mesh
- `GET /api/files` - List all stored files with metadata
- `GET /api/files/:path` - Download file by path
- `POST /api/gossip/broadcast` - Broadcast custom message
- `GET /api/metrics` - Prometheus metrics
- `WS /ws/live` - Real-time WebSocket updates

For complete documentation, see [docs/WEB_INTERFACE.md](docs/WEB_INTERFACE.md).

---

## 🌐 PODMS Scaling (EXPERIMENTAL)

> **⚠️ WARNING:** PODMS features are experimental proofs-of-concept. Not production ready.

### Policy Compiler Intelligence — Experimental Implementation

**Policy-Orchestrated Disaggregated Mesh Scaling (PODMS)** is SPACE's experimental distributed scaling model.

**Current Status:** 🟠 Experimental / 🟡 Alpha
- Basic types and telemetry infrastructure implemented
- Policy compiler can translate events to actions
- Metro-sync replication is a TCP-based proof-of-concept
- Limited real-world testing and validation

The vision: Capsules with **swarm intelligence** that self-replicate, migrate, and transform based on policy rules and real-time telemetry.

**Reality:** Core concepts are implemented but need extensive testing, performance optimization, and production hardening.

### ⚡ Quick Enable

```bash
# Build with PODMS metro-sync replication enabled
cargo build --features podms

# Run PODMS tests (includes metro-sync integration tests)
cargo test --features podms

# Run metro-sync specific tests
cargo test --features podms podms_metro_sync

# (Linux optional) Enable Phase C RDMA zero-copy transport
cargo build -p scaling --features "podms,rdma"

# (Linux optional) Spin up SoftRoCE for CI/local validation
sudo scripts/setup_softroce.sh eth0
```
Production wiring: set `SPACE_METADATA_PATH`, `SPACE_NVRAM_PATH`, and either `SPACE_MASTER_KEY` (64-hex) or `SPACE_MASTER_KEY_FILE`, then build agents via `capsule_registry::runtime::RuntimeHandles::from_env()` so `ScalingAgent::with_runtime` uses real registry/log/key-manager handles.

### 🎯 Key Features (Step 3)

- **🧠 Policy Compiler**: Translates declarative policies into executable scaling actions
- **🐝 Swarm Intelligence**: Capsules self-adapt (migrate, replicate, transform) based on telemetry
- **⚡ Autonomous Actions**: Heat spikes → migrations, capacity thresholds → rebalancing
- **🔄 Smart Replication**: RPO-driven strategies (metro-sync, async batching, none)
- **Automation Handlers**: Migration, evacuation, and rebalancing stream replication frames with MAC validation (enable via `ScalingAgent::with_runtime`)
- **🔒 Sovereignty Enforcement**: Policies block actions that violate zone constraints
- **🎭 On-the-Fly Transformation**: Re-encrypt/recompress during migrations
- **📡 Telemetry Events**: Real-time capsule lifecycle events for autonomous agents
- **🔗 Mesh Networking**: Gossip-based peer discovery with RDMA-ready zero-copy transport (Phase C) — see `docs/specs/PHASE_C_RDMA_TRANSPORT.md`, `docs/specs/EXECUTION_PLAN_PHASE_C_RDMA_SYSTEM_HARDENING.md`, and `docs/specs/PHASE_C_IMPLEMENTATION_DETAILS.md`
- **🛡️ Zero-Disruption**: Single-node mode has zero overhead (feature-gated)

### 🗺️ Scaling Policies

<div align="center">

| Policy | RPO | Latency | Sovereignty | Use Case |
|:------:|:---:|:-------:|:-----------:|:---------|
| **Metro-sync** | 0ms (sync) | 2ms | Zone | Low-latency critical data |
| **Geo-replicated** | 5min | 100ms | Global | Global availability |
| **Edge-optimized** | 5min | 50ms | Local | Edge computing |

</div>

```rust
// Metro-sync: Low latency, synchronous replication
let policy = Policy::metro_sync();

// Geo-replicated: Higher latency, async replication
let policy = Policy::geo_replicated();

// Edge-optimized: Local-only, no replication
let policy = Policy::edge_optimized();
```

### 📊 What Works Today (Step 3 Complete)

**Step 1 - Bedrock:**
- ✅ PODMS types (NodeId, ZoneId, SovereigntyLevel, Telemetry)
- ✅ Policy extensions (RPO, latency_target, sovereignty)
- ✅ Telemetry channel infrastructure
- ✅ Async event emission on capsule writes

**Step 2 - Metro-Sync Replication:**
- ✅ **Gossip discovery plane** with shared PeerStore + 1s heartbeats (control-plane mesh)
- ✅ **MeshNode data plane** still uses manual peer registration (gossip integration pending)
- ✅ **RDMA mock transport** for zero-copy segment mirroring (TCP POC)
- ✅ **Metro-sync replication** with hash-first dedup checking
- ✅ **Autonomous scaling agents** consuming telemetry events
- ✅ **Hash-based dedup preservation** during replication (32-byte hash pre-check)
- ✅ **Async batching queue** for geo-replication with configurable RPO intervals
- ✅ **ReplicationFrame protocol** with length-prefixed bincode frames
- ✅ **MAC validation** and encryption metadata during transport

**Step 3 - Policy Compiler & Execution:**
- ✅ **PolicyCompiler** translating telemetry events into ScalingActions
- ✅ **ScalingAction types**: Replicate, Migrate, Evacuate, Rebalance
- ✅ **SwarmBehavior trait** for capsule self-transformation
- ✅ **Decision rules**: RPO → replication strategy, latency → placement
- ✅ **Sovereignty validation** preventing policy violations
- ✅ **Agent execution layer** with metro-sync and async replication
- ✅ **Dedup-preserving mirror protocol** (hash-first, skip on hit)
- ✅ **Batch queue** for async geo-replication (5-min batching)
- ✅ **Comprehensive tests** (90%+ coverage on compiler logic)

### 🔜 PODMS Roadmap

- **Step 3.5** — Gossip-based peer discovery (control plane done; MeshNode pending)
- **Step 4** — Full mesh federation & cross-zone routing with Raft
- **Future** — Adaptive RPO, cost-aware placement, ML-driven heatmaps

### 🚀 Testing Multi-Node Replication

```bash
# Phase 3: metadata mesh (Raft + gossip) smoke test (3 nodes, leader failover)
./scripts/test_federation_resilience.sh

# Golden Path: UI + S3 + tiering + mesh + WASM transforms (Windows: run via Git Bash)
./scripts/test_golden_path.sh
```

📚 See [docs/podms.md](docs/podms.md) for PODMS architecture details.
📚 See [docs/guides/MESH_CLUSTER.md](docs/guides/MESH_CLUSTER.md) for Phase 3 Mesh workflows (`spacectl server` + `spacectl registry`).

---

## ✨ Development Phases

> **Reality Check:** Phase labels reflect initial goals, not current maturity. See [Feature Status Table](#-feature--capability-status) for actual implementation state.

<details open>
<summary><b>📦 Phase 1: Core Storage</b> 🟢 Beta</summary>

**Status:** Core functionality implemented and relatively stable

- ✅ Universal Capsule IDs (128-bit UUIDs)
- ✅ Persistent NVRAM Log with automatic fsync
- ✅ Intelligent 4MB Segmentation
- ✅ CLI Tool for create/read operations
- ✅ Sled-backed Metadata Registry

**Reality:** This is the most stable part of SPACE. Basic storage operations work well for single-node use.

</details>

<details open>
<summary><b>🗜️ Phase 2.1: Compression</b> 🟢 Beta</summary>

**Status:** Working well, tested

- ✅ **LZ4** — Sub-millisecond compression for hot data
- ✅ **Zstd** — High compression ratios for cold data
- ✅ **Entropy Detection** — Skip compression on random data
- ✅ **Policy-Driven** — Configure per capsule
- ✅ **Zero-Copy Fast-Path** — Borrow slices to avoid allocations

**Reality:** Compression is functional and delivers good results. Performance not fully optimized.

</details>

<details open>
<summary><b>🔗 Phase 2.2: Deduplication</b> 🟢 Beta</summary>

**Status:** Core functionality works

- ✅ **BLAKE3 Content Hashing** — Content-addressed storage
- ✅ **Automatic Dedup** — Reuse identical segments
- ✅ **Space Savings Tracking** — Monitor dedup ratios
- ✅ **Post-Compression Dedup** — Foundation for encrypted dedup
- ✅ **Zero-Copy Buffers** — Flow through hashing without cloning

**Reality:** Dedup works for single-node scenarios. Cross-node dedup is experimental.

</details>

<details open>
<summary><b>🌐 Phase 2.3: Protocol Views</b> 🟡 Alpha / 🟠 Experimental</summary>

**Status:** S3 is alpha, NFS/Block are experimental

- 🟡 **S3 REST API** — Basic PUT/GET/HEAD/LIST/DELETE (incomplete, no multipart)
- 🟠 **NFS Namespace** — Experimental namespace implementation
- 🟠 **Block Volumes** — Prototype LUN facade with COW
- 🟡 **Protocol Abstraction** — Basic framework exists

**Reality:** S3 API has basic functionality but missing features. NFS/Block are early prototypes with minimal testing.

</details>

<details open>
<summary><b>🔐 Phase 3.1: Encryption & Integrity</b> 🟢 Beta / 🟡 Alpha</summary>

**Status:** Basic encryption works, advanced features need testing

- ✅ **XTS-AES-256** — Per-segment encryption implemented
- ✅ **BLAKE3-MAC** — Tamper detection with keyed MAC
- 🟡 **Deterministic Encryption** — Preserves dedup, needs more testing
- 🟡 **Key Management** — Basic derivation/rotation, limited validation
- ✅ **Zero-Trust Design** — Keys from environment, zeroized on drop

**Reality:** Core encryption works but needs security audit. Key rotation and deterministic encryption need more real-world testing.

</details>

<details open>
<summary><b>🛡️ Phase 3.3: Advanced Security</b> 🟡 Alpha / 🟠 Experimental</summary>

**Status:** Features implemented but need thorough security review

- 🟡 **Counting Bloom Filters** — Basic implementation works
- 🟡 **Immutable Audit Log** — BLAKE3 chaining works, TSA hooks stubbed
- 🟠 **Zero-Trust Ingress** — SPIFFE + mTLS eBPF hooks experimental
- 🟠 **Post-Quantum Crypto** — Kyber toggle exists, untested
- 🟡 **Modular Security** — Feature-gated code organization

**Reality:** Security features need professional audit and extensive testing before any production consideration.

</details>

<details open>
<summary><b>✨ Phase 4: Protocol Views + Full Mesh Federation</b> 🟠 Experimental</summary>

**Status:** Implemented as a simulation-first “View” layer (feature-gated), with an experimental read-only kernel FUSE mount on Unix

- 🟠 **NVMe / NFS / CSI projection helpers** – Feature-gated adapters exist (`protocol-nvme`, `protocol-nfs::phase4`, `protocol-csi`)
- 🟠 **Local projection mount** – `spacectl project mount` can use an experimental read-only kernel FUSE mount on Unix (enable `spacectl` feature `kernel_fuse` + install `libfuse3-dev`), with a portable `content`-file view fallback elsewhere
- 🟠 **Federation (Phase 4b, gRPC)** – `Policy.federation.targets` triggers async replication via `spacectl zone add` + `spacectl federation serve`
- 🟠 **Policy-orchestrated mobility** – Views invoke `scaling::enforce_view_policy` before projection
- 📄 See [docs/phase4.md](docs/phase4.md) for current behavior + limitations

**Reality:** Phase 4 is still experimental, but it is no longer “docs-only”: you can create a capsule, mount a view, and (optionally) replicate into another zone without changing client tooling.

</details>

<details open>
<summary><b>🧠 Phase 5: The Brain (Compute-over-Data / WASM)</b> 🟡 Planned / 🟠 Experimental</summary>

**Status:** Policy schema + initial runtime integration implemented (early/experimental)

- 🟡 **Transform policy** – `Policy.transform` defines an ordered chain of WASM transforms with triggers (`on-read` / `on-write`)
- 🟡 **Sandboxed execution** – WASM modules run inside wasmtime with fuel + memory limits (traps fail the read/write, not the node)
- 🟠 **Streaming reads** – transforms wrap `read_capsule_stream` so clients see transformed bytes without pre-materializing derived objects
- 🟡 **Dogfooding** – `capsule://...` images are intended to load WASM binaries stored in SPACE itself
- 📄 See [docs/phase5.md](docs/phase5.md) for the schema + ABI
- 🛠️ Build: `cargo build -p spacectl --features phase5` (enables modular pipeline + WASM transforms)

**Reality:** This phase targets compute-to-data primitives first; derived-output caching and richer ABIs evolve next.

</details>

---

## 🚀 Quick Start

### 🐳 Docker Quick Start (90 Seconds)

Run a 3-node encrypted dedup S3 cluster in under 90 seconds:

```bash
# Clone and run
git clone https://github.com/saworbit/SPACE && cd SPACE
docker compose -f containerization/docker-compose.yml up -d

# S3 endpoint ready at http://localhost:8080
# Test upload
curl -X PUT --data-binary @myfile.bin http://localhost:8080/bucket/myfile.bin
```

#### 🔧 Troubleshooting Cheat-Sheet
```bash
# Build stuck?
docker builder prune

# Port 8080 busy?
docker compose down

# Hugepages error?
sudo sysctl vm.nr_hugepages=128

# View logs
docker compose logs -f node1

# Rebuild fresh
docker compose build --no-cache
```

#### 💡 Pro Tips
1. **Policy Injection** — Mount policies: `-v ./policies.toml:/capsules/policies.toml`
2. **Zero-Downtime Rollout** — `docker compose up -d --no-deps --build node1`
3. **Air-gapped Deploy** — `docker save space-core | gzip > space.tar.gz`

---

### 💻 System Requirements

<div align="center">

| Requirement | Version/Details |
|:-----------:|:---------------:|
| 🐧 **OS** | Linux or Windows (macOS not supported¹) |
| 🦀 **Rust** | 1.88+ |
| 💾 **Disk** | 2GB free space |

</div>

> ¹ **macOS Platform Status**: macOS is not currently supported due to systematic storage backend data integrity issues. All foundry storage tests fail on macOS with data corruption (reading zeros instead of written data). Root cause appears to be platform-specific incompatibilities with sparse file handling and direct I/O operations. Future macOS support would require significant platform-specific storage layer work.

### 🔨 Build

```bash
cargo build --release
```

### 🐳 Development Setup with Simulations *(Recommended for Testing)*

SPACE includes Docker-based simulations for testing without physical hardware:

```bash
# Quick setup: Build images and start environment
./scripts/setup_home_lab_sim.sh

# View running services
docker compose ps

# Run end-to-end tests
./scripts/test_e2e_sim.sh

# View simulation logs
docker compose logs -f sim

# Stop environment
docker compose down
```

**What you get**:
- ✅ **NVRAM simulation**: File-backed log for testing pipeline
- ✅ **NVMe-oF simulation**: Native NVMe/TCP target with optional SPDK feature gating and automatic fallback; ships `nvme-cli` helper scripts
- ✅ **Foundry NVMe-oF**: Expose Foundry volumes via `spacectl expose` with SPDK async bridge (Milestone 8.2)
- ✅ **Multi-node setup**: Simulate distributed capsule mesh

**For more details**:
- 📘 [SIMULATIONS.md](docs/SIMULATIONS.md): Detailed simulation guide
- 🐳 [CONTAINERIZATION.md](docs/CONTAINERIZATION.md): Docker architecture
- 🧪 Run tests: `cargo test -p sim-nvram -p capsule-registry --test pipeline_sim_integration`
- Validate NVMe/TCP path with nvme-cli: ./scripts/nvmeof_discover.sh (discover) and sudo ./scripts/nvmeof_connect_io.sh (connect + 4KiB I/O)

### 🔐 Setup Encryption *(Optional)*

```bash
# Option A: master key via env (64 hex chars)
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

# Verify setup
echo ${#SPACE_MASTER_KEY}  # Should output 64

# Option B: master key via file (e.g., Docker secret)
# export SPACE_MASTER_KEY_FILE=/run/secrets/space_master_key
```

### 🛡️ Advanced Security Setup *(Optional)*
```bash
# Opt-in to Bloom/audit/SPIFFE/PQ via the feature flag
cargo build --features advanced-security

# Registry tuning (optional)
export SPACE_BLOOM_CAPACITY=10000000        # default: 10M entries
export SPACE_BLOOM_FPR=0.001                # default: 0.1% false positives

# Audit log (optional TSA batches every 100 events)
export SPACE_AUDIT_LOG=/var/lib/space/space.audit.log
export SPACE_AUDIT_FLUSH=5                  # fsync every 5 events
export SPACE_TSA_ENDPOINT=https://tsa.local/submit
export SPACE_TSA_API_KEY=demo-token

# SPIFFE + mTLS ingress (protocol-s3)
export SPACE_ALLOWED_SPIFFE_IDS="spiffe://demo/client-a,spiffe://demo/client-b"
export SPACE_SPIFFE_ENDPOINT=ws://127.0.0.1:9001/identities
export SPACE_SPIFFE_HEADER=x-spiffe-id
export SPACE_SPIFFE_REFRESH_SECS=30
export SPACE_BPF_PROGRAM=/opt/space/gateway.bpf.o   # optional on Linux

# Kyber hybrid toggle for PQ readiness
export SPACE_KYBER_KEY_PATH=/var/lib/space/space.kyber.key
```

Run the zero-trust S3 test on Linux (aya/ebpf requires a unix target):
```bash
cargo test -p protocol-s3 --features advanced-security
```

### 📝 Create Your First Capsule

```bash
# Create a test file
echo "Hello SPACE!" > test.txt

# Create a capsule
./target/release/spacectl create --file test.txt
```

**Output:**
```
✅ Capsule created: 550e8400-e29b-41d4-a716-446655440000
   Size: 13 bytes
   Segment 0: 1.85x compression (13 -> 7 bytes, lz4_1)
   1.85x compression, 0 dedup hits
```

### 📖 Read It Back

```bash
./target/release/spacectl read 550e8400-e29b-41d4-a716-446655440000 > output.txt
```

### 🔗 Test Deduplication
```bash
# Create file with repeated content (Bash)
echo "SPACE STORAGE " > test_repeated.txt
for i in {1..5000}; do echo "SPACE STORAGE " >> test_repeated.txt; done

# PowerShell alternative:
# "SPACE STORAGE " * 5000 | Out-File test_repeated.txt

# Create first capsule
./target/release/spacectl create --file test_repeated.txt

# Create second capsule (same content - watch for dedup!)
./target/release/spacectl create --file test_repeated.txt

# Expected Output:
# *  Dedup hit: Reusing segment 1 (saved 4194304 bytes)
# [x] Capsule ...: 5.23x compression, 1 dedup hits (4194304 bytes saved)
```

### ⚡ Enable Async Pipeline & Metrics (optional)
```bash
# Build with async pipeline enabled
cargo build --features pipeline_async

# Run CLI with runtime-managed async pipeline and info-level tracing
RUST_LOG=info ./target/debug/spacectl create --file test.txt

# Run feature-gated tests
cargo test -p capsule-registry --features pipeline_async
```

### 🧩 Opt in to the Modular Pipeline (compression/dedup/encryption traits)
```bash
# Build everything with the modular orchestrator available
cargo build --features modular_pipeline

# Create or read capsules via the trait-based pipeline
./target/release/spacectl create --file demo.txt --modular
./target/release/spacectl read 550e8400-e29b-41d4-a716-446655440000 --modular > output.txt

# Serve the S3 view against the modular backend
./target/release/spacectl serve-s3 --port 8080 --modular

# Legacy callers can still flip back at runtime, even when the feature is enabled
SPACE_DISABLE_MODULAR_PIPELINE=1 ./target/release/spacectl create --file demo.txt
```

The modular path instantiates `compression`, `dedup`, `encryption`, and `storage` crates through shared traits, while `WritePipeline` now selects the orchestrator at runtime (Strategy pattern): when the `modular_pipeline` feature is compiled in, it prefers the modular backend unless `SPACE_DISABLE_MODULAR_PIPELINE=1` is set; you can force delegation with `SPACE_USE_MODULAR=1`, and it falls back to the legacy path if initialization fails. Protocol crates (e.g., S3) and the CLI share a common helper (`registry_pipeline_from_env`) so they all exercise the same code paths. Disable the feature entirely for leaner binaries via `--no-default-features` or by omitting `--features modular_pipeline`.

### 🌐 Start S3 Server
```bash
./target/release/spacectl serve-s3 --port 8080

# In another terminal, test S3 API
curl -X PUT http://localhost:8080/demo-bucket/hello.txt -d "Hello from S3!"
curl http://localhost:8080/demo-bucket/hello.txt
```

---

## 🏗️ Architecture

### System Overview

```
╔══════════════════════════════════════════════════════════╗
║                  💻 spacectl (CLI)                       ║
║           Your interface to the storage fabric           ║
╚══════════════════════════╦═══════════════════════════════╝
                           ║
╔══════════════════════════╩═══════════════════════════════╗
║              📋 CapsuleRegistry                          ║
║      Metadata & Segment Mappings                         ║
║      Content Store: ContentHash → SegmentId              ║
╠══════════════════════════════════════════════════════════╣
║              ⚙️ WritePipeline                            ║
║   Segment → Compress → Hash → Encrypt → MAC → Dedup     ║
╚══════════════════════════╦═══════════════════════════════╝
                           ║
╔══════════════════════════╩═══════════════════════════════╗
║                 💾 NvramLog                              ║
║         Durable append-only segment storage              ║
╚══════════════════════════════════════════════════════════╝
```

### 🔄 Write Pipeline Data Flow

```
📄 Input File
   │
   ├─➤ Split into 4MB segments
   │
   ├─➤ 🗜️ Compress (LZ4/Zstd)
   │   └─➤ Skip if high entropy
   │
   ├─➤ #️⃣ Hash (BLAKE3)
   │
   ├─➤ 🔐 Encrypt (XTS-AES-256)
   │   ├─➤ Derive deterministic tweak from hash
   │   └─➤ Preserves deduplication
   │
   ├─➤ ✅ Compute MAC (BLAKE3-keyed)
   │
   ├─➤ 🔍 Check Content Store
   │   ├─➤ Hit?  ➜ Reuse existing segment (dedup!)
   │   └─➤ Miss? ➜ Write new segment
   │
   ├─➤ 💾 Append to NVRAM log (fsync)
   │
   ├─➤ 📋 Update Metadata Registry
   │
   └─➤ ✨ Return CapsuleID
```

---

## 📁 Project Structure
```
space/
+-- crates/
|   +-- common/              # Shared types (CapsuleId, SegmentId, Policy)
|   +-- encryption/          # NEW: XTS-AES-256 + BLAKE3-MAC + Key management
|   |   +-- src/
|   |   |   +-- lib.rs       # Module exports
|   |   |   +-- error.rs     # Error types
|   |   |   +-- policy.rs    # EncryptionPolicy & metadata
|   |   |   +-- keymanager.rs# Key derivation & rotation
|   |   |   +-- xts.rs       # XTS-AES-256 encryption
|   |   |   +-- mac.rs       # BLAKE3-MAC integrity
|   |   +-- tests/           # 53 passing tests
|   +-- capsule-registry/    # Metadata + write pipeline + dedup + encryption
|   |   +-- src/
|   |   |   +-- lib.rs       # Registry with content store
|   |   |   +-- pipeline.rs  # Write/read with encryption integration
|   |   |   +-- compression.rs # LZ4/Zstd adaptive compression
|   |   |   +-- dedup.rs     # BLAKE3 hashing & stats
|   |   +-- tests/
|   |       +-- integration_test.rs
|   |       +-- dedup_test.rs
|   +-- nvram-sim/           # Persistent log storage simulator
|   +-- protocol-s3/         # S3-compatible REST API
|   +-- spacectl/            # Command-line interface
+-- docs/
|   +-- architecture.md
|   +-- patentable_concepts.md
|   +-- future_state_architecture.md
|   +-- guides/
|   |   +-- BUILD.md                     # Build + test instructions
|   |   +-- QUICKSTART_S3.md             # Protocol view demo
|   +-- implementation/
|   |   +-- DEDUP_IMPLEMENTATION.md      # Phase 2.2 details
|   |   +-- ENCRYPTION_IMPLEMENTATION.md # NEW: Phase 3 details
|   |   +-- IMPLEMENTATION_COMPLETE.md   # Replication runbook
|   |   +-- IMPLEMENTATION_SUMMARY.md    # Implementation overview
|   +-- status/
|   |   +-- INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md # Progress tracking
|   |   +-- MULTI_NODE_STATUS.md                         # Multi-node readiness
|   +-- ...                                              # See docs/README.md for full index
+-- scripts/
|   +-- clean.sh
|   +-- demo_s3.sh
|   +-- setup_home_lab_sim.sh
|   +-- sim-entrypoint.sh
|   +-- test_dedup.sh
|   +-- test_encryption.sh
|   +-- test_e2e_sim.sh
|   +-- test_federation_failover.sh
|   +-- test_federation_resilience.sh
|   +-- test_phase4.sh
|   +-- test_phase4_views.sh
+-- UI_mockup/             # Orbit command interface mock (Vite) - see docs/guides/UI_MOCKUP.md
+-- Cargo.toml               # Workspace configuration
+-- README.md                # You are here
```

### ⚙️ Runtime Files (Auto-Generated)
```
space.db               -> Capsule registry + content store (sled)
space.nvram            -> Raw segment data (encrypted if enabled)
space.nvram.segments   -> Segment metadata with encryption info (JSON)
```

---

## 🧪 Testing

### Run Tests

```bash
# Run all tests
cargo test --workspace

# Run with output (see compression/dedup/encryption stats)
cargo test --workspace -- --nocapture

# Run specific test suites
cargo test -p encryption -- --nocapture
cargo test -p protocol-s3 -- --nocapture
cargo test -p scaling --test replication_integration -- --nocapture  # Inbound replication persistence/dedup/MAC coverage
cargo test --features advanced-security -- --nocapture
./scripts/test_batch_queue_limits.sh  # BatchQueue byte/count/stat limits

# Automated dedup demo
./scripts/test_dedup.sh  # Linux/macOS/Git Bash
```

### ✅ Test Coverage

<div align="center">

| Feature | Status |
|:--------|:------:|
| Write/read round-trip | ✅ |
| Multi-segment handling | ✅ |
| Metadata persistence | ✅ |
| NVRAM log recovery | ✅ |
| Compression entropy detection | ✅ |
| Deduplication across capsules | ✅ |
| S3 protocol views | ✅ |
| Encryption/decryption | ✅ |
| MAC integrity verification | ✅ |
| Key derivation & rotation | ✅ |
| Deterministic encryption | ✅ |

</div>

---

## 🚀 Control Plane API

- Versioned REST surface lives under `/api/v1` across `system`, `mesh`, `data`, and `gossip` domains.
- Standard response envelope (`success`, `data`, `error`, `meta`) with pagination metadata; Swagger UI at `/swagger-ui`, spec at `/api-docs/openapi.json`.
- JWT guard with RBAC (`admin`, `editor`, `viewer`); `system/health` stays public for probes; set `JWT_SECRET` or `GOSSIP_SIGNING_KEY`.
- Streaming multipart uploads replace base64 for `POST /api/v1/data/objects`; downloads support `GET|HEAD`.
- See `docs/SPACE_CONTROL_PLANE_API.md` and `docs/WEB_INTERFACE.md` for usage examples.
- Dev auth helpers: `scripts/dev_auth.sh` mints HS256 tokens; debug builds use a random ephemeral JWT secret (set `JWT_SECRET` to override). The god-token shortcut requires `SPACE_DEV_GOD_TOKEN` to be explicitly set as an env var (no hardcoded default).

### 🔧 Full-feature linting (LibTorch)

- The `layout-engine` `ml` feature needs LibTorch 2.2.0. To run `cargo clippy --all-features` locally:
  1. Download `libtorch-win-shared-with-deps-2.2.0+cpu.zip` from https://download.pytorch.org/libtorch/cpu/.
  2. Extract and set `LIBTORCH=C\path\to\libtorch`.
  3. Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- `cargo xtask audit` will warn if LibTorch is missing; set `XTASK_STRICT_LIBTORCH=1` in CI to enforce.

---

## 💡 Why This Matters

### The Problem with Traditional Storage

<div align="center">

| ⚠️ Problem | ✅ SPACE Solution |
|:-----------|:------------------|
| 🔒 Protocol lock-in | **One capsule, multiple views** |
| 📦 Data duplication | **Content-addressed deduplication** |
| 🔄 Complex migrations | **Instant protocol switching** |
| 🚚 Forklift upgrades | **Microservice evolution** |
| 🛡️ Bolt-on security | **Built-in per-segment encryption** |
| 🔐 Encryption kills dedup | **Deterministic tweaks preserve dedup** |
| 💾 Wasted space | **Automatic 2-3x savings** |
| ⚡ CPU overhead | **Entropy detection skips random data** |
| ✔️ No integrity checks | **BLAKE3-MAC on every segment** |

</div>

### 🎯 Proven Innovations

<div align="center">

| Innovation | Status | Impact |
|:-----------|:------:|:-------|
| 🔐 **Dedup Over Encrypted Data** | ✅ | Deterministic encryption preserves efficiency |
| 🗜️ **Adaptive Compression** | ✅ | LZ4/Zstd with entropy-based selection |
| #️⃣ **Content-Addressed Storage** | ✅ | BLAKE3 hashing enables global dedup |
| 🌐 **Protocol Views** | ✅ | Universal namespace with S3/NFS/Block |
| 💾 **Space Efficiency** | ✅ | 2-3x savings maintained with encryption |
| 🔑 **Key Management** | ✅ | Version-tracked derivation with rotation |
| ✅ **Integrity Verification** | ✅ | BLAKE3-MAC detects tampering |

</div>

---

## 🔐 Security & Encryption

### 💎 The Core Innovation

<table>
<tr>
<td width="50%">

**❌ Traditional Encryption**
```
Plaintext A + Random IV
   ↓
Ciphertext X

Plaintext A + Random IV
   ↓
Ciphertext Y (different!)

Result: Dedup FAILS ❌
```

</td>
<td width="50%">

**✅ SPACE's Breakthrough**
```
Plaintext A → Compress → Hash
   ↓ Deterministic Tweak
Ciphertext X

Plaintext A → Compress → Hash
   ↓ Same Tweak
Ciphertext X

Result: Dedup WORKS! 🎉
```

</td>
</tr>
</table>

### 🛡️ Security Properties

<div align="center">

| Property | Implementation | Strength |
|:---------|:--------------:|:--------:|
| 🔒 **Confidentiality** | XTS-AES-256 | 256-bit |
| ✅ **Integrity** | BLAKE3-MAC | 128-bit |
| 🔗 **Deduplication** | Deterministic tweaks | ✅ Preserved |
| 🔑 **Key Derivation** | BLAKE3-KDF | Cryptographic |
| 🔄 **Key Rotation** | Version tracking | Zero downtime |
| 🧹 **Memory Safety** | Zeroization | Secure |
| 🛡️ **Path Traversal** | Null/backslash/`..` rejection | Hardened |
| 🔓 **No Hardcoded Secrets** | Random ephemeral keys | Hardened |
| 📦 **Frame Size Limits** | 16 MiB deserialization cap | DoS-resistant |
| 🔄 **Mutex Recovery** | `unwrap_or_else` poisoning | Resilient |
| 🔒 **TLS Infrastructure** | mTLS env-based config | Ready |

</div>

### ⚡ Quick Encryption Setup

```bash
# Option A: 256-bit master key via env (64 hex chars)
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

# Option B: master key via file (e.g., Docker secret)
# export SPACE_MASTER_KEY_FILE=/run/secrets/space_master_key

# Encryption now auto-enabled! ✨
```

📚 **Detailed documentation:** [ENCRYPTION_IMPLEMENTATION.md](docs/implementation/ENCRYPTION_IMPLEMENTATION.md)

---

## 🗺️ Roadmap

### ✅ Phase 1: Core Storage (COMPLETE)
- ✅ Capsule registry with persistent metadata
- ✅ NVRAM log simulator
- ✅ CLI for create/read operations
- ✅ 4MB automatic segmentation
- ✅ Integration tests

### ✅ Phase 2.1: Compression (COMPLETE)
- ✅ LZ4 fast compression
- ✅ Zstd balanced compression
- ✅ Entropy-based compression selection
- ✅ Policy-driven compression levels
- ✅ Compression statistics tracking

### ✅ Phase 2.2: Deduplication (COMPLETE)
- ✅ BLAKE3 content hashing
- ✅ Content-addressed storage (ContentHash -> SegmentId)
- ✅ Post-compression deduplication
- ✅ Dedup statistics and monitoring
- ✅ Reference counting (foundation for GC)

### ✅ Phase 2.3: Protocol Views (COMPLETE)
- ✅ S3-compatible REST API
- ✅ PUT/GET/HEAD/LIST/DELETE operations
- ✅ Protocol abstraction layer
- ✅ S3 server with Axum

### ✅ Phase 3.1: Encryption & Integrity (COMPLETE)
- ✅ XTS-AES-256 per-segment encryption
- ✅ Deterministic tweak derivation (preserves dedup)
- ✅ BLAKE3-MAC integrity verification
- ✅ Key management with BLAKE3-KDF
- ✅ Key rotation with version tracking
- ✅ Environment-based key configuration
- ✅ Memory zeroization for security
- ✅ 53 comprehensive tests

### ✅ Phase 3.2: Lifecycle Management (COMPLETE)
- ✅ Reference-counted segment tracking across capsules
- ✅ Startup refcount reconciliation on pipeline initialization
- ✅ Manual garbage collector for metadata reclamation

### ✅ Phase 3.3: Advanced Security (COMPLETE)
- ✅ Counting Bloom filters + registry plumbing
- ✅ Immutable audit log with BLAKE3 hash chains + TSA hooks
- ✅ SPIFFE + mTLS ingress middleware + refreshable allow-list
- ✅ Kyber hybrid crypto profile + segment metadata
- ✅ Security module + docs aligning Bloom/Audit/PQ/eBPF

### 🔮 Phase 4: Advanced Protocol Views
- 📋 NVMe-oF block target (SPDK feature-gated, TCP fallback)
- 📋 NFS v4.2 file export
- 📋 FUSE filesystem mount
- 📋 CSI driver for Kubernetes
- Encryption-transparent views via RegistryTransformOps + centralized enforce_view_policy so protocols serve plaintext while capsules stay XTS-encrypted

### 🧠 Phase 5: The Brain (Compute-over-Data)
- 🟠 WASM transform engine embedded in the pipeline (`Policy.transform`)
- 📋 Chained transforms with resource limits (fuel + memory pages)
- 📋 On-read transforms for streaming clients (no pre-processing storage cost)
- 📋 On-write transforms for destructive ingest filtering (optional)

### 🚀 Phase 6: Enterprise Features
- 📋 Metro-sync replication
- 📋 Autonomous tiering (hot/cold) + rehydrate
- 📋 Policy compiler
- 📋 Erasure coding (6+2)
- 📋 Hardware offload (DPU/GPU)
- 📋 Confidential compute enclaves

---

## ⚡ Performance

### 🗜️ Compression Performance

<div align="center">

| Data Type | Algorithm | Compression | Throughput |
|:----------|:---------:|:-----------:|:----------:|
| 📝 **Text/Logs** | Zstd-3 | 3-5x | ~500 MB/s |
| 📦 **Binary** | LZ4-1 | 1.5-2.5x | ~2 GB/s |
| 🎲 **Random** | None | 1.0x | ~5 GB/s |

</div>

### 🔗 Deduplication Ratios

<div align="center">

| Scenario | Dedup Ratio | Space Saved |
|:---------|:-----------:|:-----------:|
| 💿 **VM Images** | 10-20x | 90-95% |
| 📋 **Log Files** | 2-5x | 50-80% |
| 👤 **User Data** | 1.5-3x | 30-65% |
| ✨ **Unique Data** | 1.0x | 0% |

</div>

### 🔐 Encryption Overhead

<div align="center">

| Operation | Baseline | With Encryption | Overhead |
|:---------:|:--------:|:---------------:|:--------:|
| **Write** | 2.1 GB/s | 2.0 GB/s | +5% |
| **Read** | 3.5 GB/s | 3.2 GB/s | +9% |
| **Dedup** | ✅ Works | ✅ **Still Works** | **0%** |

</div>

### 📊 Per-Segment Breakdown (4MB)

```
🗜️  Compression (LZ4)    ~0.5ms   2.5 GB/s
#️⃣  Hashing (BLAKE3)     ~0.3ms   13 GB/s
🔐 Encryption (XTS-AES) ~0.8ms   5 GB/s (AES-NI)
✅ MAC (BLAKE3)         ~0.3ms   13 GB/s
💾 NVRAM write          ~0.1ms   (fsync)
──────────────────────────────────────────
⚡ Total                ~2.0ms per segment
```

### 📈 Total Overhead

<div align="center">

**Combined pipeline overhead: <10% increase in write latency**

</div>

### Async Pipeline Runtime Overhead

- **Global Tokio runtime for sync bridge:** `WritePipeline` now reuses a single background runtime (OnceLock) instead of creating a new `Runtime` per call, removing millisecond-scale latency spikes in hot paths. See `docs/specs/PERFORMANCE_FIX_PIPELINE_RUNTIME.md`.
- **Benchmark proof:** `cargo bench --bench runtime_overhead` compares "New Runtime per Call" vs "Global Runtime" (expect ~100x faster sync calls).
- **BatchQueue hybrid flush:** Async replication queue now enforces both count and byte ceilings (default 4MiB helper) to prevent OOM from oversized payloads; verify via `./scripts/test_batch_queue_limits.sh`.

### �YO? Zero-Copy Replication (Linux)

- **tokio-uring actor data plane**: Linux builds pin a ring thread that behaves as a transport actor with per-peer persistent TCP connections and multiplexed writes; non-Linux keeps the Tokio TCP fallback.
- **Backpressure-aware**: Bounded queue logs warnings above 80% utilization and backpressures when full to keep the control plane responsive.
- **Probe script**: `./scripts/replication_io_uring_smoke.sh` (Linux) runs the `uring_probe` example and surfaces queue-depth logs; tune load with `FRAME_COUNT` and `FRAME_BYTES`.

---

## 🤝 Contributing

<div align="center">

**⚠️ Pre-Alpha Research Project — Contributions Welcome with Realistic Expectations**

</div>

### Understanding SPACE's Current State

SPACE is a **single-developer research project** exploring novel storage architectures. Many features are proofs-of-concept or aspirational designs. If you're interested in contributing, please understand:

- **Not production software** - This is experimental research
- **APIs will change** - No stability guarantees
- **Documentation aspirational** - Many docs describe goals, not current reality
- **Limited resources** - Single developer means slow review/merge cycles
- **Learning opportunity** - Great for exploring storage systems architecture

### How to Contribute

**We Welcome (with realistic expectations):**
- 🐛 **Bug reports** - Help identify issues in existing features
- 💡 **Architecture discussions** - Join the exploration of new ideas
- 📚 **Documentation clarifications** - Help align docs with reality
- 🧪 **Test improvements** - Expand coverage of existing features
- ⚡ **Performance analysis** - Profile and identify bottlenecks
- 🔒 **Security reviews** - Expert review of crypto/security implementations

**Contribution Guidelines:**
1. ✨ Run `cargo fmt` and `cargo clippy`
2. ✅ Ensure `cargo test --workspace` passes
3. 📖 Update documentation to match reality
4. 🧪 Add tests for new functionality
5. ⏰ **Be patient** - single developer means slower response times

**Good First Issues:**
- Improving test coverage on core storage features
- Clarifying documentation (especially "planned" vs "implemented")
- Adding error handling to existing code paths
- Performance benchmarking and profiling

📄 See [CONTRIBUTING.md](CONTRIBUTING.md) • [Code of Conduct](CODE_OF_CONDUCT.md) • [Security](SECURITY.md)

---

## 📚 Documentation

<div align="center">

| Document | Description |
|:---------|:------------|
| 🏗️ [Architecture Overview](docs/architecture.md) | Full system design |
| 🔮 [Future State Architecture](docs/future_state_architecture.md) | Vision and roadmap |
| 💡 [Patentable Concepts](docs/patentable_concepts.md) | Novel mechanisms |
| 🔗 [Dedup Implementation](docs/implementation/DEDUP_IMPLEMENTATION.md) | Phase 2.2 technical details |
| 🔐 [Encryption Implementation](docs/implementation/ENCRYPTION_IMPLEMENTATION.md) | Phase 3 security details |
| 🔗 [Implementation Summary](docs/implementation/IMPLEMENTATION_SUMMARY.md) | Cross-cutting milestones |
| 🔗 [Inbound Replication Status](docs/status/INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md) | Progress tracking |
| 🔗 [Multi-Node Status](docs/status/MULTI_NODE_STATUS.md) | Federation readiness |
| 🌐 [Protocol Views](docs/protocol_views.md) | S3/NFS/block facades |
| 🧪 [Simulations Guide](docs/SIMULATIONS.md) | Testing without hardware |
| 🐳 [Containerization Guide](docs/CONTAINERIZATION.md) | Docker deployment |
| 🚀 [S3 Quick Start](docs/guides/QUICKSTART_S3.md) | Protocol view demo |
| 🎨 [UI Mockup Walkthrough](docs/guides/UI_MOCKUP.md) | Launch the Orbit command interface concept |
| 🔨 [Build Guide](docs/guides/BUILD.md) | Compilation and testing |
| [Streaming Reads & Pagination](docs/guides/STREAMING_READS.md) | Zero-copy capsule reads + cursor listings |

</div>

---

## 📜 License

<div align="center">

**Dual Licensed: MIT OR Apache 2.0**

SPACE is dual-licensed under your choice of either:
- **MIT License** ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- **Apache License 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

This follows the same licensing model as the Rust programming language itself.

✅ **Commercial use allowed** • 📝 **Retain attribution** • 🤝 **Contributions welcome**

**Why dual license?** You can choose whichever license works best for your project. Both are permissive open source licenses, but Apache 2.0 includes an explicit patent grant which provides additional legal protection.

[📄 MIT License](LICENSE-MIT) • [📄 Apache 2.0 License](LICENSE-APACHE) • [🤝 Contributing Guide](CONTRIBUTING.md)

</div>

---

## 📊 Project Status

<div align="center">

> ### ⚠️ PRE-ALPHA RESEARCH PROJECT
>
> **NOT PRODUCTION READY - FOR RESEARCH AND EXPERIMENTATION ONLY**

| Aspect | Status |
|:-------|:-------|
| **🎯 Maturity Level** | **Pre-Alpha** (v0.1.0) |
| **🔬 Stability** | **Unstable** — Breaking changes without notice |
| **🚀 Production Use** | **❌ NOT RECOMMENDED** — Educational/research only |
| **👤 Development** | Single developer, limited real-world testing |
| **🧪 Test Coverage** | ~70-80%, many gaps remain |
| **📚 Documentation** | Describes vision more than current reality |

</div>

### Reality Check: What Actually Works vs. What's Documented

This project has **extensive documentation describing future vision and design goals**, but many documented features are experimental, incomplete, or proof-of-concept only.

**📊 See the [Feature & Capability Status Table](#-feature--capability-status) above for detailed maturity levels.**

### ✅ What's Actually Solid (Beta Quality)

- **Core capsule storage:** Create/read operations with persistent metadata
- **Compression:** LZ4/Zstd with entropy detection works well
- **Deduplication:** BLAKE3 content-addressed storage functional
- **Basic encryption:** XTS-AES-256 per-segment encryption implemented
- **CLI basics:** spacectl create/read commands work
- **NVRAM simulation:** File-backed testing environment

### 🟡 What's Alpha/Experimental (Use With Caution)

- **S3 API:** Basic operations work but incomplete (no multipart, limited error handling)
- **Multi-node features:** PODMS mesh/replication are proofs-of-concept, not production-ready
- **Advanced security:** Bloom filters, audit logs, SPIFFE/mTLS need thorough testing
- **Web interface:** Basic dashboard exists but limited functionality
- **NFS/Block views:** Experimental prototypes with minimal testing

### 🔴 What's Planned But Not Implemented

- **Full Kubernetes integration:** CSI is still a helper/stub; no in-tree driver deployment yet
- **Kernel-backed mounts:** read-only FUSE is now available on Unix (experimental); NBD remains unimplemented
- **Full mesh federation:** global routing/sharding is still evolving beyond the Phase 4b gRPC bridge
- **Raft consensus:** Capsule metadata Raft exists (Phase 3) but is still experimental; Phase 4 federation/sharding remains planned/stubbed
- **Production features:** Robust error recovery, monitoring, backup/restore, etc.

### ⚠️ Critical Limitations

- **No production validation:** Single-developer project, limited real-world use
- **API instability:** Breaking changes expected as design evolves
- **Performance:** Not optimized, benchmarks incomplete
- **Security:** Features need professional audit before any production consideration
- **Multi-node:** Experimental distributed features not battle-tested
- **Data safety:** Do not trust with important data
- **Documentation gaps:** Some docs describe aspirational architecture, not current implementation

---

## 🎬 Quick Demo

### Basic Usage

```bash
# Build SPACE
cargo build --release

# Optional: Enable encryption (choose one)
export SPACE_MASTER_KEY=$(openssl rand -hex 32)
# export SPACE_MASTER_KEY_FILE=/run/secrets/space_master_key

# Create a file with repeated content
echo "SPACE STORAGE PLATFORM" > demo.txt
for i in {1..1000}; do echo "SPACE STORAGE PLATFORM" >> demo.txt; done

# First capsule (establishes baseline)
./target/release/spacectl create --file demo.txt

# Second capsule (watch dedup in action!)
./target/release/spacectl create --file demo.txt
```

**Expected Output:**
```
✨ Dedup hit: Reusing segment 0 (saved 24576 bytes)
🔐 Segment 1: encrypted with key v1
✅ Capsule ...: 5.2x compression, 1 dedup hits (24576 bytes saved)
```

### S3 Protocol Demo

```bash
# Start S3 server
./target/release/spacectl serve-s3 --port 8080 &

# Store object via S3 API
curl -X PUT http://localhost:8080/demo/test.txt -d "Hello SPACE!"

# Retrieve object
curl http://localhost:8080/demo/test.txt
```

### 📂 Explore NFS and Block views
```powershell
# Create directories and write a file via the NFS view
spacectl nfs mkdir --path /lab/results
spacectl nfs write --path /lab/results/report.json --file report.json
spacectl nfs list --path /lab/results
spacectl nfs read --path /lab/results/report.json > fetched.json

# Provision a 32MiB block volume and write a sector
spacectl block create vol1 33554432
spacectl block write vol1 4096 --file sector.bin
spacectl block read vol1 4096 --length 512 > sector.verify
spacectl block delete vol1
```

### 📊 Advanced: Telemetry & Error Handling

**Environment Variables:**
- `SPACE_LOG_FORMAT` — Console output format (`compact` or `json`)
- `RUST_LOG` — Tracing filters (e.g., `RUST_LOG=info,space=debug`)

**Pipeline Events:**
- All stages emit structured spans (`pipeline::compression`, `telemetry::compression`)

**Error Reference:**

<div align="center">

| Error Code | Level | Description | Action |
|:-----------|:-----:|:------------|:-------|
| `CompressionError::EntropySkip` | WARN | High-entropy data skipped | Review if persistent |
| `CompressionError::IneffectiveRatio` | INFO | Compression reverted | Tune thresholds |
| `PipelineError::Compression` | ERROR | Compression failed | Retry/inspect codec |
| `PipelineError::Nvram` | ERROR | Storage I/O failure | Check backing store |
| `PipelineError::Telemetry` | WARN | Telemetry rejected | Check hub health |

</div>

---

<div align="center">

## 🌟 Support SPACE

**⭐ Star us on GitHub if you find this research project interesting! ⭐**

> **Note:** This is a pre-alpha research project. Star it to follow development, but please read the [Feature Status Table](#-feature--capability-status) to understand current capabilities vs. documented vision.

[🐛 Report Bug](https://github.com/saworbit/SPACE/issues) • [💡 Discuss Ideas](https://github.com/saworbit/SPACE/issues) • [📚 Improve Docs](https://github.com/saworbit/SPACE/issues)

---

**Built with 🦀 Rust** • **Pre-Alpha Research Project** • **Not Production Ready**

*Exploring novel storage architectures — one capsule at a time.*

**Current Status (see [Feature Status](#-feature--capability-status) for details):**
- 🟢 Core Storage: Beta quality
- 🟡 Protocol Views: Alpha/Experimental
- 🟠 Multi-Node: Experimental proof-of-concept
- 🟠 Federation: Experimental (gRPC WAN bridge)

---

**© 2024 SPACE Project** • Licensed under [MIT](LICENSE-MIT) OR [Apache 2.0](LICENSE-APACHE)

**⚠️ Use at your own risk** • Educational and research purposes only • Not production ready

</div>

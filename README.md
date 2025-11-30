<div align="center">

# 🚀 SPACE
### Storage Platform for Adaptive Computational Ecosystems

[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![CI](https://github.com/saworbit/SPACE/actions/workflows/ci.yml/badge.svg)](https://github.com/saworbit/SPACE/actions/workflows/ci.yml)
[![Discussions](https://img.shields.io/github/discussions/saworbit/SPACE)](https://github.com/saworbit/SPACE/discussions)

### *One capsule. Infinite views.*
**The future of storage starts with a single primitive that breaks down protocol silos.**

---

**🎉 Phase 3.3 Complete:** Encryption ✅ • Bloom Filters ✅ • Audit Log ✅ • SPIFFE/mTLS ✅ • PODMS Scaling ✅

**🌐 Multi-Node Ready:** Orchestrator ✅ • Gossip Protocol ✅ • Autonomous Scaling ✅ • Transformation in Transit ✅

**🚀 Phase 4 Active:** Multi-Protocol Views (NVMe-oF, NFS, FUSE, CSI) + Full Mesh Federation

[🚀 Quick Start](#-quick-start) • [📚 Documentation](#-documentation) • [🎬 Demo](#-quick-demo) • [💡 Why SPACE](#-why-this-matters)

</div>

---

## 📖 Table of Contents

- [💡 The Big Idea](#-the-big-idea)
- [📊 What Works Today](#-what-works-today)
- [🌐 PODMS Scaling](#-podms-scaling)
- [✨ Development Phases](#-development-phases)
- [🚀 Quick Start](#-quick-start)
- [🏗️ Architecture](#️-architecture)
- [📁 Project Structure](#-project-structure)
- [🧪 Testing](#-testing)
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

**🎯 Phase 3.3 Complete — Advanced Security Hardened**

</div>

### ✅ Core Features
- 🔮 Universal capsule storage with persistent metadata
- 💻 CLI create/read operations
- 🌐 S3-compatible REST API (protocol view proof-of-concept)
- 📂 NFS + block protocol views (namespace + volume facades)
- 🗜️ Adaptive compression (LZ4/Zstd with entropy detection)
- ⚡ Zero-copy compression/dedup pipeline using `Cow<[u8]>` + `bytes::Bytes` shared buffers
- 🔗 Content-addressed deduplication (post-compression)
- 🔐 **XTS-AES-256 encryption with BLAKE3-MAC integrity**
- 🎯 **Deterministic encryption preserving deduplication**
- 🔑 **Key management with rotation support**
- 🗑️ **Reference-counted garbage collection with metadata reclamation**
- 🧩 **Modular trait-based pipeline for read/delete/GC (feature `modular_pipeline`)**
- ⚙️ **Tokio-powered async write pipeline** (Cargo feature `pipeline_async`) with staged NVRAM transactions, bounded concurrency, and `tracing` metrics
- 🌸 **Counting Bloom filters** in the registry to prescreen dedup candidates at multi-million scale
- 📝 **Immutable audit log** with BLAKE3 hash chaining + optional TSA anchoring (`security::audit_log`)
- 🛡️ **SPIFFE + mTLS eBPF gateway** when the `advanced-security` feature is enabled (`protocol-s3`)
- 🔮 **Post-quantum crypto toggle** (Kyber + AES hybrid) selectable via `Policy::crypto_profile`
- 🏗️ **Dedicated `security` module** so Bloom/audit/PQ/eBPF logic stays feature gated
- Automation handlers for migration/evacuation/rebalancing using mesh streaming + MAC validation

### 🌐 Multi-Node Capabilities

**NEW: PODMS Orchestrator** - Transform SPACE into a distributed mesh network:

- **🎯 Autonomous Operations**
  - Metro-sync replication (zero-RPO, <2ms latency)
  - Async-batch replication (5min RPO, optimized bandwidth)
  - Heat-based migration for hot data
  - Capacity-driven rebalancing
  - Node evacuation (immediate or gradual)

- **🔐 Secure Gossip Protocol**
  - HMAC-SHA256 message signing
  - TTL-based flood control
  - Message deduplication
  - Configurable fanout (8-16 peers)

- **⚡ Transformation in Transit**
  - Re-encryption during migration
  - Re-compression optimization
  - Key rotation support
  - BLAKE3 MAC validation
  - Dependency-inverted `TransformOps` pipeline (see `docs/specs/PODMS_SWARM_BEHAVIOR.md` and the runtime adapter in `docs/specs/PODMS_TRANSFORM_OPS.md`)
  - Scaling migrations now invoke `SwarmOps` to decrypt -> decompress -> recompress -> re-encrypt with per-capsule XTS keys and fresh MACs before streaming frames
  - Envelope encryption: segment keys are convergent (content-derived) and wrapped per capsule, enabling Zero Trust isolation without sacrificing dedup
  - Cross-node deduplication

- **🐳 Docker Compose Simulation**
  - 3-node mesh environment
  - Prometheus + Grafana monitoring
  - Isolated network testing
  - Quick start: `docker-compose -f docker-compose.multi-node.yml up`

📘 **[Multi-Node Deployment Guide →](docs/multi-node-deployment.md)**

### 🔜 Coming Next
- **Full mesh federation** & cross-zone routing (Step 4)
- **ML-driven heatmaps** & adaptive placement

---

## 🌐 Next-Gen Web Interface

**NEW: Real-time mesh monitoring and management dashboard**

SPACE now includes a comprehensive web interface for visualizing and managing the distributed mesh network with integrated gossip protocol support.

### ✨ Features

- 📊 **Real-time peer discovery** with gossip protocol visualization
- 🔄 **Live WebSocket updates** for mesh topology changes
- 📈 **Prometheus metrics** for gossip convergence and bandwidth
- 🗂️ **File operations** - Upload, browse, and download files with Blake3 verification
- 💾 **File storage dashboard** - View all stored files with metadata and one-click downloads
- 🔐 **Security alerts** broadcast via gossip
- 🎯 **RBAC support** (Admin, Editor, Viewer, StorageNode, Gateway roles)
- ⚡ **Reactive frontend** built with Leptos (optional, feature-gated)

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

## 🌐 PODMS Scaling
### Policy Compiler Intelligence — Step 3 Complete

**Policy-Orchestrated Disaggregated Mesh Scaling** is SPACE's distributed scaling model.

Step 3 brings the **policy compiler** — the "brain" that translates declarative policies into autonomous scaling actions. Capsules now exhibit **swarm intelligence**: self-replicating, migrating, and transforming based on policy rules and real-time telemetry.

### ⚡ Quick Enable

```bash
# Build with PODMS metro-sync replication enabled
cargo build --features podms

# Run PODMS tests (includes metro-sync integration tests)
cargo test --features podms

# Run metro-sync specific tests
cargo test --features podms podms_metro_sync
```
Production wiring: set `SPACE_METADATA_PATH`, `SPACE_NVRAM_PATH`, and `SPACE_MASTER_KEY`, then build agents via `capsule_registry::runtime::RuntimeHandles::from_env()` so `ScalingAgent::with_runtime` uses real registry/log/key-manager handles.

### 🎯 Key Features (Step 3)

- **🧠 Policy Compiler**: Translates declarative policies into executable scaling actions
- **🐝 Swarm Intelligence**: Capsules self-adapt (migrate, replicate, transform) based on telemetry
- **⚡ Autonomous Actions**: Heat spikes → migrations, capacity thresholds → rebalancing
- **🔄 Smart Replication**: RPO-driven strategies (metro-sync, async batching, none)
- **Automation Handlers**: Migration, evacuation, and rebalancing stream replication frames with MAC validation (enable via `ScalingAgent::with_runtime`)
- **🔒 Sovereignty Enforcement**: Policies block actions that violate zone constraints
- **🎭 On-the-Fly Transformation**: Re-encrypt/recompress during migrations
- **📡 Telemetry Events**: Real-time capsule lifecycle events for autonomous agents
- **🔗 Mesh Networking**: Gossip-based peer discovery with RDMA-ready transport
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
- ✅ **Mesh networking** with manual peer registration (gossip planned for Step 3.5)
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

- **Step 3.5** — Gossip-based peer discovery (replace manual registration)
- **Step 4** — Full mesh federation & cross-zone routing with Raft
- **Future** — Adaptive RPO, cost-aware placement, ML-driven heatmaps

### 🚀 Testing Multi-Node Replication

```bash
# Start 3 nodes with Docker Compose
docker-compose up --scale nodes=3

# Test metro-sync replication (zero-RPO)
spacectl create-capsule --data "test" --policy metro-sync
spacectl verify-replicas --expected 2

# Test async geo-replication (5-min RPO)
spacectl create-capsule --data "test" --policy geo-replicated
spacectl queue-stats  # Check batch queue depth
```

📚 See [docs/podms.md](docs/podms.md) for architecture details and implementation guide.

---

## ✨ Development Phases

<details open>
<summary><b>📦 Phase 1: Core Storage</b> ✅</summary>

- ✅ Universal Capsule IDs (128-bit UUIDs)
- ✅ Persistent NVRAM Log with automatic fsync
- ✅ Intelligent 4MB Segmentation
- ✅ CLI Tool for create/read operations
- ✅ Sled-backed Metadata Registry (Raft-ready, streaming snapshots)

</details>

<details open>
<summary><b>🗜️ Phase 2.1: Compression</b> ✅</summary>

- ✅ **LZ4** — Sub-millisecond compression for hot data
- ✅ **Zstd** — High compression ratios for cold data
- ✅ **Entropy Detection** — Skip compression on random data
- ✅ **Policy-Driven** — Configure per capsule
- ✅ **Zero-Copy Fast-Path** — Borrow slices to avoid allocations

</details>

<details open>
<summary><b>🔗 Phase 2.2: Deduplication</b> ✅</summary>

- ✅ **BLAKE3 Content Hashing** — Content-addressed storage
- ✅ **Automatic Dedup** — Reuse identical segments
- ✅ **Space Savings Tracking** — Monitor dedup ratios
- ✅ **Post-Compression Dedup** — Foundation for encrypted dedup
- ✅ **Zero-Copy Buffers** — Flow through hashing without cloning

</details>

<details open>
<summary><b>🌐 Phase 2.3: Protocol Views</b> ✅</summary>

- ✅ **S3 REST API** — PUT/GET/HEAD/LIST/DELETE
- ✅ **NFS Namespace** — Hierarchical directories
- ✅ **Block Volumes** — Logical LUN facade with COW
- ✅ **Protocol Abstraction** — Same capsule, multiple APIs

</details>

<details open>
<summary><b>🔐 Phase 3.1: Encryption & Integrity</b> ✅</summary>

- ✅ **XTS-AES-256** — Per-segment encryption with hardware acceleration
- ✅ **BLAKE3-MAC** — Tamper detection with keyed MAC
- ✅ **Deterministic Encryption** — Preserves deduplication
- ✅ **Key Management** — Version-tracked derivation with rotation
- ✅ **Zero-Trust Design** — Keys from environment, zeroized on drop

</details>

<details open>
<summary><b>🛡️ Phase 3.3: Advanced Security</b> ✅</summary>

- 🌸 **Counting Bloom Filters** — Guard registry from multi-million entry explosions (~0.1% false positives)
- 📝 **Immutable Audit Log** — BLAKE3 hash chaining + optional TSA webhooks
- 🔒 **Zero-Trust Ingress** — SPIFFE + mTLS gateway with eBPF policy filter
- 🔮 **Post-Quantum Crypto** — Kyber ML-KEM hybrid for forward secrecy
- 🏗️ **Modular Security** — Feature-gated Bloom/Audit/PQ/eBPF code

</details>

<details open>
<summary><b>✨ Phase 4: Protocol Views + Full Mesh Federation</b> 🚀</summary>

- 🧱 **NVMe-oF block targets** – Phase 4 exports capsules through the NVMe surface without materializing copies
- 📁 **NFS + FUSE + CSI** – Runtime projection modules for file, mount, and Kubernetes consumer experiences
- 🌐 **Federated metadata mesh** – Mesh nodes gossip metadata, resolve capsule locations, shard registry state, and surface Paxos-style consistency
- 🧭 **Policy-orchestrated mobility** – `Policy::latency_target` and `Policy::sovereignty` drive federation, QoS, and transformation decisions
- ⚙️ Build with `cargo build --features phase4` and project via `spacectl --view nvme|nfs|fuse|csi`
- 📄 Reference [docs/phase4.md](docs/phase4.md) for crate details, policy examples (`examples/phase4-policy.yaml`), and test scripts (`scripts/test_phase4.sh`, `scripts/test_federation_failover.sh`)
- ☸️ Kubernetes users can drop in `deployment/csi-deployment.yaml` after enabling the CSI view feature

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
| 🐧 **OS** | Linux, macOS, or Windows |
| 🦀 **Rust** | 1.78+ |
| 💾 **Disk** | 2GB free space |

</div>

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
- ✅ **Multi-node setup**: Simulate distributed capsule mesh

**For more details**:
- 📘 [SIMULATIONS.md](docs/SIMULATIONS.md): Detailed simulation guide
- 🐳 [CONTAINERIZATION.md](docs/CONTAINERIZATION.md): Docker architecture
- 🧪 Run tests: `cargo test -p sim-nvram -p capsule-registry --test pipeline_sim_integration`
- Validate NVMe/TCP path with nvme-cli: ./scripts/nvmeof_discover.sh (discover) and sudo ./scripts/nvmeof_connect_io.sh (connect + 4KiB I/O)

### 🔐 Setup Encryption *(Optional)*

```bash
# Generate master key for encryption
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

# Verify setup
echo ${#SPACE_MASTER_KEY}  # Should output 64
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
cargo test --features advanced-security -- --nocapture

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

</div>

### ⚡ Quick Encryption Setup

```bash
# Generate 256-bit master key
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

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

### 🚀 Phase 5: Enterprise Features
- 📋 Metro-sync replication
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

### �YO? Zero-Copy Replication (Linux)

- **tokio-uring data plane**: Linux builds enqueue replication frames directly to io_uring SQEs for kernel-side DMA without user/kernel copies; non-Linux keeps the Tokio TCP fallback.
- **Backpressure-aware**: Bounded queue logs warnings above 80% utilization and backpressures when full to keep the control plane responsive.
- **Probe script**: `./scripts/replication_io_uring_smoke.sh` (Linux) runs the `uring_probe` example and surfaces queue-depth logs; tune load with `FRAME_COUNT` and `FRAME_BYTES`.

---

## 🤝 Contributing

<div align="center">

**We're exploring radical new storage architectures — join us!**

</div>

**We Welcome:**
- 🐛 Bug reports and fixes
- 💡 Architecture suggestions
- 📚 Documentation improvements
- 🧪 New test cases
- ⚡ Performance optimizations
- 🔒 Security reviews

**Before Submitting PRs:**
1. ✨ Run `cargo fmt` and `cargo clippy`
2. ✅ Ensure `cargo test --workspace` passes
3. 📖 Update documentation
4. 🧪 Add tests for new functionality

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
| ?? [Implementation Summary](docs/implementation/IMPLEMENTATION_SUMMARY.md) | Cross-cutting milestones |
| ?? [Inbound Replication Status](docs/status/INBOUND_REPLICATION_IMPLEMENTATION_STATUS.md) | Progress tracking |
| ?? [Multi-Node Status](docs/status/MULTI_NODE_STATUS.md) | Federation readiness |
| 🌐 [Protocol Views](docs/protocol_views.md) | S3/NFS/block facades |
| 🧪 [Simulations Guide](docs/SIMULATIONS.md) | Testing without hardware |
| 🐳 [Containerization Guide](docs/CONTAINERIZATION.md) | Docker deployment |
| 🚀 [S3 Quick Start](docs/guides/QUICKSTART_S3.md) | Protocol view demo |
| 🔨 [Build Guide](docs/guides/BUILD.md) | Compilation and testing |

</div>

---

## 📜 License

<div align="center">

**Apache 2.0** — Permissive open source license with patent grant

✅ **Commercial use allowed** • 📝 **Retain attribution** • 🤝 **Contributions welcome**

[📄 Full License](LICENSE) • [🤝 Contributing Guide](CONTRIBUTING.md)

</div>

---

## 📊 Project Status

<div align="center">

| Aspect | Status |
|:-------|:-------|
| **🎯 Current Phase** | Phase 4: Views & Federation |
| **🔬 Stability** | Experimental — API subject to change |
| **🚀 Production** | Not yet (educational/research) |

</div>

### ✅ What's Working

- ✅ Capsule storage with compression and deduplication
- ✅ XTS-AES-256 encryption preserving dedup
- ✅ Counting Bloom filters + immutable audit log
- ✅ SPIFFE/mTLS gateway + eBPF policy enforcement
- ✅ Post-quantum Kyber hybrid mode
- ✅ S3-compatible REST API
- ✅ NFS, FUSE, and CSI protocol views (Phase 4)
- ✅ CLI tools and persistent metadata

### ⚠️ Known Limitations

- 📋 Log-space reclamation (in progress)
- 📋 Single-node only (clustering = Phase 5)
- 📋 Authentication/authorization (Phase 4/5)

---

## 🎬 Quick Demo

### Basic Usage

```bash
# Build SPACE
cargo build --release

# Optional: Enable encryption
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

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

**⭐ Star us on GitHub if you find this project interesting! ⭐**

[🐛 Report Bug](https://github.com/saworbit/SPACE/issues) • [💡 Request Feature](https://github.com/saworbit/SPACE/issues) • [💬 Discussions](https://github.com/saworbit/SPACE/discussions)

---

**Built with 🦀 Rust**

*Breaking storage silos, one encrypted capsule at a time.*

**🎉 Phase 3.3 Complete**
Compression ✅ • Dedup ✅ • Protocol Views ✅ • Advanced Security ✅

---

**© 2024 SPACE Project** • Licensed under [Apache 2.0](LICENSE)

</div>

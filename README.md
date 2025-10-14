# 🚀 SPACE MVP - Storage Platform for Adaptive Computational Ecosystems

> **One capsule. Infinite views.** The future of storage starts with a single primitive that breaks down protocol silos.

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-Early%20MVP-yellow.svg)](https://github.com/your-org/space)

---

## 💡 The Big Idea

Traditional storage forces you into boxes: **block** *or* **file** *or* **object**. Different APIs, separate data copies, endless complexity.

**SPACE flips the script.** Everything is a **capsule** — a universal 128-bit ID that can be viewed through *any* protocol:

```
┌─────────────────────────────────────┐
│   The Same Capsule, Three Views     │
├─────────────────────────────────────┤
│  📦 Block    →  NVMe-oF, iSCSI      │
│  📄 File     →  NFS, SMB            │
│  ☁️  Object   →  S3 API              │
└─────────────────────────────────────┘
```

No copies. No conversions. Just pure, protocol-agnostic storage.

---

## ✨ What This MVP Proves

**Status:** Phase 1 Complete — Core storage layer working!

✅ **Universal Capsule IDs** — 128-bit UUIDs as the single storage primitive  
✅ **Persistent NVRAM Log** — Append-only durability with automatic fsync  
✅ **Intelligent Segmentation** — Auto-split to 4MB chunks for efficiency  
✅ **CLI Tool** — Create and read capsules from the command line  
✅ **JSON Metadata** — Human-readable registry for debugging and inspection  

---

## 🎯 Quick Start

### Build
```bash
cargo build --release
```

### Create a Capsule
```bash
# From a file
echo "Hello SPACE!" > test.txt
./target/release/spacectl create --file test.txt
```

**Output:**
```
✅ Capsule created: 550e8400-e29b-41d4-a716-446655440000
   Size: 13 bytes
```

### Read It Back
```bash
# Replace UUID with your capsule ID
./target/release/spacectl read 550e8400-e29b-41d4-a716-446655440000 > output.txt
```

### Test Multi-Segment Storage
```bash
# Create 10MB file (3 segments @ 4MB each)
dd if=/dev/urandom of=bigfile.bin bs=1M count=10

./target/release/spacectl create --file bigfile.bin
./target/release/spacectl read <capsule-uuid> > bigfile_out.bin

# Verify integrity
diff bigfile.bin bigfile_out.bin
```

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────┐
│                    spacectl (CLI)                   │
│              Your interface to the fabric           │
└────────────────────┬────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────┐
│              CapsuleRegistry                        │
│    Manages capsule metadata & segment mappings     │
├─────────────────────────────────────────────────────┤
│              WritePipeline                          │
│    Segments data → Encrypts → Dedupes → Stores     │
└────────────────────┬────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────┐
│                 NvramLog                            │
│        Durable append-only segment storage          │
└─────────────────────────────────────────────────────┘
```

### Data Flow (Write Path)

```
Input File
    │
    ├─► Split into 4MB segments
    │
    ├─► Generate SegmentID
    │
    ├─► Append to NVRAM log (fsync)
    │
    └─► Update metadata registry
         │
         └─► Return CapsuleID to user
```

---

## 📁 Project Structure

```
space/
├── crates/
│   ├── common/              # Shared types (CapsuleId, SegmentId, Segment)
│   ├── capsule-registry/    # Metadata + write pipeline
│   ├── nvram-sim/           # Persistent log storage simulator
│   └── spacectl/            # Command-line interface
├── docs/
│   ├── docs_architecture.md # Full system design
│   └── docs_patentable_concepts.md
├── Cargo.toml               # Workspace configuration
└── README.md                # You are here
```

### Runtime Files (Auto-Generated)

```
space.metadata         → Capsule-to-Segment mappings (JSON)
space.nvram            → Raw segment data (binary)
space.nvram.segments   → Segment offset index (JSON)
```

---

## 🧪 Testing

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Integration tests only
cargo test --test integration_test
```

**Test Coverage:**
- ✅ Write/read round-trip
- ✅ Multi-segment handling
- ✅ Metadata persistence
- ✅ NVRAM log recovery

---

## 🎨 Why This Matters

### Traditional Storage Problems

| Problem | SPACE Solution |
|---------|----------------|
| Protocol lock-in (block vs file vs object) | **One capsule, multiple views** |
| Data duplication across tiers | **Single source of truth** |
| Complex migration between protocols | **Instant protocol switching** |
| Forklift upgrades required | **Microservice-based evolution** |
| Security bolted on afterward | **Built-in encryption per segment** |

### Future-Ready Architecture

This MVP proves the core storage abstraction. Coming soon:

🔐 **Per-segment encryption** (XTS-AES-256)  
🗜️ **Adaptive compression** (LZ4/Zstd based on entropy)  
📊 **Deduplication** (GPU-accelerated bloom filters)  
⚡ **Protocol views** (NVMe-oF, NFS, S3)  
🌐 **Replication** (Metro-sync, async fan-out)  

---

## 🗺️ Roadmap

### ✅ Phase 1: Core Storage (COMPLETE)
- [x] Capsule registry with persistent metadata
- [x] NVRAM log simulator
- [x] CLI for create/read operations
- [x] 4MB automatic segmentation
- [x] Integration tests

### 🚧 Phase 2: Space Efficiency (IN PROGRESS)
- [ ] List and delete commands
- [ ] LZ4/Zstd adaptive compression
- [ ] XTS-AES-256 encryption per segment
- [ ] Range reads for block semantics
- [ ] Basic deduplication

### 🔮 Phase 3: Protocol Views
- [ ] NVMe-oF block target (SPDK)
- [ ] NFS v4.2 file export
- [ ] S3-compatible object API
- [ ] CSI driver for Kubernetes

### 🌟 Phase 4: Enterprise Features
- [ ] Metro-sync replication
- [ ] Policy compiler
- [ ] Erasure coding (6+2)
- [ ] Hardware offload (DPU/GPU)
- [ ] Confidential compute enclaves

---

## 🤝 Contributing

This is an experimental platform exploring radical new storage architectures. We welcome:

- 🐛 Bug reports and fixes
- 💡 Architecture suggestions
- 📖 Documentation improvements
- 🧪 New test cases

**Before submitting PRs:**
1. Run `cargo fmt` and `cargo clippy`
2. Ensure all tests pass
3. Update documentation for new features

---

## 📚 Learn More

- **[Architecture Overview](docs/architecture.md)** — Full system design
- **[Patentable Concepts](docs/patentable_concepts.md)** — Novel mechanisms
- **[API Documentation](https://docs.rs/space)** — Coming soon

---

## 📄 License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE) or http://www.apache.org/licenses/LICENSE-2.0)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work shall be licensed as above, without any additional terms or conditions.

---

## 🎯 Project Status

**Current Phase:** Early MVP  
**Stability:** Experimental — API subject to change  
**Production Ready:** Not yet (educational/research purposes)

---

<div align="center">

**Built with 🦀 Rust**

*Breaking storage silos, one capsule at a time.*

[Report Bug](https://github.com/your-org/space/issues) · [Request Feature](https://github.com/your-org/space/issues) · [Discussions](https://github.com/your-org/space/discussions)

</div>
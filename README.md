# SPACE MVP - Storage Platform for Adaptive Computational Ecosystems

A minimal viable product demonstrating SPACE's core concept: **universal capsule-based storage**.

## What Works (MVP v0.1)

✅ Universal 128-bit capsule IDs  
✅ Persistent NVRAM log storage  
✅ Automatic 4MB segmentation  
✅ CLI for create/read operations  
✅ JSON-based metadata registry  

## Quick Start

Build the project:
cargo build --release

Create a capsule:
.\target\release\spacectl.exe create --file mydata.txt

Read it back (replace UUID with output from create):
.\target\release\spacectl.exe read <uuid> > output.txt

## Architecture

   ┌─────────────┐
   │   spacectl  │  CLI tool
   └──────┬──────┘
          │
   ┌──────▼──────────────┐
   │ CapsuleRegistry     │  Maps IDs → Segments
   ├─────────────────────┤
   │ WritePipeline       │  Segment + Store
   └──────┬──────────────┘
          │
   ┌──────▼──────────────┐
   │ NvramLog            │  Append-only log
   └─────────────────────┘

## Project Structure

space/
├── crates/
│   ├── common/              # Shared types (CapsuleId, Segment)
│   ├── capsule-registry/    # Registry + pipeline
│   ├── nvram-sim/           # Persistent log simulator
│   └── spacectl/            # CLI tool
└── Cargo.toml               # Workspace root

## Runtime Files

- space.metadata - Capsule registry (JSON)
- space.nvram - Raw data segments
- space.nvram.segments - Segment offset map (JSON)

## Testing

Run tests:
cargo test

Test multi-segment (10MB file, 3 segments):
fsutil file createnew bigfile.bin 10485760
.\target\release\spacectl.exe create --file bigfile.bin
.\target\release\spacectl.exe read <uuid> > bigfile_out.bin

## What Makes This Different

Traditional storage systems force you to choose: block, file, or object. SPACE uses **capsules** - a single universal primitive that can be viewed through any protocol:

- Same capsule via NVMe-oF (block)
- Same capsule via NFS (file)  
- Same capsule via S3 (object)

This MVP proves the core storage layer works. Protocol views come next.

## Roadmap

### Phase 1 (Current MVP)
- [x] Capsule registry with persistent metadata
- [x] NVRAM log simulator
- [x] Basic CLI (create/read)
- [x] 4MB segmentation

### Phase 2 (Next)
- [ ] List/delete commands
- [ ] Compression (LZ4/Zstd adaptive)
- [ ] Encryption (XTS-AES-256 per segment)
- [ ] Range reads for block semantics

### Phase 3 (Protocol Views)
- [ ] NVMe-oF target (SPDK)
- [ ] NFS v4.2 export
- [ ] S3-compatible API

### Phase 4 (Advanced)
- [ ] Replication (metro-sync)
- [ ] Policy compiler
- [ ] Erasure coding
- [ ] Hardware offload (DPU/GPU)

## Contributing

This is an experimental storage platform exploring new architectures. Contributions welcome, but expect breaking changes as the design evolves.

## License

Apache 2.0

## References

- Design documentation in repository
- Full architecture specs available

---

Status: Early MVP - Proves core capsule concept works  
Author: Shane Wall  
Created: October 2025
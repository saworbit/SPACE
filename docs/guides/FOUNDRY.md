# Phase 8: The Foundry - Polymorphic Block Storage

## Overview

The Foundry is SPACE's high-performance mutable block storage layer that provides a unified abstraction over different storage backends. It enables volume-level operations with pluggable implementations optimized for different deployment scenarios.

**Status:** 🟢 Beta (LegacyBackend) / 🟠 Experimental (MagmaBackend)

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Application Layer                     │
│              (Protocol Views, Databases, VMs)            │
└────────────────────┬────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────┐
│                  Foundry Manager                         │
│        (Runtime Backend Selection & Registry)            │
└────────────────────┬────────────────────────────────────┘
                     │
        ┌────────────┴────────────┐
        │                         │
        ▼                         ▼
┌──────────────┐          ┌──────────────┐
│ VolumeBackend Trait     │
├──────────────┤          ├──────────────┤
│ LegacyBackend│          │ MagmaBackend │
│ (File-based) │          │(Log-struct.) │
└──────┬───────┘          └──────┬───────┘
       │                         │
       ▼                         ▼
┌──────────────┐          ┌──────────────┐
│  Filesystem  │          │ DirectIoDevice│
│ (Sparse File)│          │  (Stub/SPDK) │
└──────────────┘          └──────────────┘
```

## Key Concepts

### VolumeBackend Trait

The core abstraction that all storage backends implement:

```rust
pub trait VolumeBackend: Send + Sync {
    fn init(&self, size_bytes: u64) -> BoxFuture<'_, Result<()>>;
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, Result<Bytes>>;
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, Result<()>>;
    fn sync(&self) -> BoxFuture<'_, Result<()>>;
    fn size(&self) -> BoxFuture<'_, Result<u64>>;
    fn resize(&self, new_size: u64) -> BoxFuture<'_, Result<()>>;
}
```

**Design Choice:** Uses manual `BoxFuture` instead of `#[async_trait]` to match SPACE's existing `StorageBackend` pattern.

### Backend Types

1. **LegacyBackend** - File-based sparse volumes
   - ✅ Universal compatibility (Linux, macOS, Windows)
   - ✅ No special privileges required
   - ✅ Sparse file support via filesystem
   - ✅ Production-ready
   - ⚠️ Subject to filesystem fragmentation and write amplification

2. **MagmaBackend** - Log-structured storage
   - 🔬 Experimental (SPDK integration pending)
   - ⚡ Zero write amplification design
   - 🚀 Optimized for raw NVMe devices
   - 📊 L2P mapping for logical-to-physical translation
   - 🔄 Future: Background garbage collection

3. **DirectIoDevice** - Device abstraction
   - ⚪ Currently a stub using tokio::fs
   - 🔮 Future: SPDK NVMe bdev integration
   - 🔮 Future: io_uring with O_DIRECT

## Usage

### Basic Example

```rust
use foundry::{Foundry, BackendType, VolumeId};
use bytes::Bytes;

#[tokio::main]
async fn main() -> foundry::error::Result<()> {
    // Create Foundry instance
    let foundry = Foundry::new();

    // Create a 10MB volume
    let volume_id = VolumeId::new();
    let volume = foundry
        .create_volume(volume_id, 10 * 1024 * 1024, None)
        .await?;

    // Write data
    let data = Bytes::from(vec![0x42; 4096]);
    volume.write_at(0, data.clone()).await?;

    // Read data back
    let read_data = volume.read_at(0, 4096).await?;
    assert_eq!(read_data, data);

    // Sync to disk
    volume.sync().await?;

    // Get volume size
    let size = volume.size().await?;
    println!("Volume size: {} bytes", size);

    // Cleanup
    foundry.delete_volume(volume_id).await?;

    Ok(())
}
```

### Backend Selection

```rust
use foundry::{Foundry, BackendType};

// Force Legacy backend (guaranteed to work)
let foundry = Foundry::new()
    .with_backend(BackendType::Legacy);

// Auto-select (try Magma, fallback to Legacy)
let foundry = Foundry::new()
    .with_backend(BackendType::Auto);

// Force Magma (fail if unavailable)
let foundry = Foundry::new()
    .with_backend(BackendType::Magma);
```

### Custom Data Directory

```rust
use foundry::Foundry;
use std::path::PathBuf;

// Use custom directory
let foundry = Foundry::with_data_dir("/mnt/nvme/volumes");

// Or via environment variable
std::env::set_var("SPACE_DATA_DIR", "/mnt/nvme/volumes");
let foundry = Foundry::new();
```

### Sparse Volumes

```rust
// Create large sparse volume (only uses space for written data)
let volume_id = VolumeId::new();
let volume = foundry
    .create_volume(volume_id, 100 * 1024 * 1024 * 1024, None) // 100GB
    .await?;

// Write at the beginning
let data = Bytes::from(vec![0xAA; 4096]);
volume.write_at(0, data).await?;

// Write at the end (sparse in between)
let data = Bytes::from(vec![0xBB; 4096]);
volume.write_at(100 * 1024 * 1024 * 1024 - 4096, data).await?;

// Reading unwritten regions returns zeros
let middle = volume.read_at(50 * 1024 * 1024 * 1024, 4096).await?;
assert_eq!(middle, Bytes::from(vec![0u8; 4096]));
```

### Volume Resize

```rust
// Create 10MB volume
let volume = foundry
    .create_volume(volume_id, 10 * 1024 * 1024, None)
    .await?;

// Resize to 20MB
volume.resize(20 * 1024 * 1024).await?;

// Can now write to expanded region
let data = Bytes::from(vec![0x99; 4096]);
volume.write_at(15 * 1024 * 1024, data).await?;
```

## Snapshots

Foundry supports point-in-time snapshots using the `SnapshotEngine`. By bridging the ephemeral, high-speed world of Foundry with the immortal, deduplicated vault of the Capsule Registry, we effectively solve the "state problem" in distributed systems.

**Status:** 🟢 Beta (Milestone 8.1: The Bridge)

### Architecture

- **Chunking:** Volumes are split into 64KB blocks for efficient deduplication
- **Deduplication:** Identical blocks (e.g., zero-filled regions or common OS files) are stored only once globally via the Capsule Registry
- **Manifest:** A JSON capsule containing the map of Block Offsets -> Capsule IDs
- **Atomicity:** The Manifest is the "commit point". Until the Manifest is written, the snapshot does not technically exist

### Basic Snapshot Usage

```rust
use foundry::snapshot::SnapshotEngine;
use capsule_registry::CapsuleRegistry;
use capsule_registry::pipeline::WritePipeline;
use nvram_sim::NvramLog;
use common::Policy;
use std::sync::Arc;

// Setup snapshot infrastructure
let registry = CapsuleRegistry::open("registry.db")?;
let nvram = NvramLog::open("nvram.log")?;
let pipeline = Arc::new(WritePipeline::new(registry, nvram));
let engine = SnapshotEngine::new(pipeline);

// Take a snapshot
let manifest_id = engine.take_snapshot(
    volume_id,
    volume.clone(),
    Policy::default()
).await?;

// Later, restore from snapshot
engine.restore_snapshot(volume_id, manifest_id, volume).await?;
```

### Snapshot Policies

Snapshots respect the Policy passed during creation, enabling compression, encryption, and deduplication:

```rust
// Default policy (LZ4 compression + deduplication)
let manifest_id = engine.take_snapshot(
    volume_id,
    volume.clone(),
    Policy::default()
).await?;

// High compression for text-heavy volumes
let manifest_id = engine.take_snapshot(
    volume_id,
    volume.clone(),
    Policy::text_optimized()
).await?;

// Encrypted snapshots
let manifest_id = engine.take_snapshot(
    volume_id,
    volume.clone(),
    Policy::encrypted()
).await?;
```

### Restore to Different Volume

```rust
// Create a new empty volume
let new_volume_id = VolumeId::new();
let new_volume = foundry
    .create_volume(new_volume_id, 1, Some(BackendType::Legacy))
    .await?;

// Restore snapshot (volume will be auto-resized)
engine.restore_snapshot(new_volume_id, manifest_id, new_volume).await?;
```

### Sparse Volume Optimization

The snapshot engine handles sparse volumes efficiently:

- **Zero Blocks:** Empty regions are deduplicated into a "Global Zero Block"
- **Future:** `lseek(SEEK_DATA)` integration to skip holes entirely
- **Storage:** A 100GB sparse volume with 1MB data creates ~1MB of capsules + manifest

### Snapshot Performance

| Operation | Typical Performance | Notes |
|:----------|:-------------------|:------|
| **Snapshot (10MB)** | ~100-200ms | Depends on compression/dedup |
| **Snapshot (1GB)** | ~5-10s | Pipeline throughput ~100-200 MB/s |
| **Restore (10MB)** | ~50-100ms | Read + decompress + write |
| **Restore (1GB)** | ~3-7s | Limited by volume write speed |
| **Dedup Ratio** | 2-10x | For OS images, databases with common patterns |

### Future Enhancements

- **Incremental Snapshots:** Only snapshot changed blocks since last snapshot
- **Copy-on-Write:** Instant snapshots with lazy copying
- **Snapshot Chains:** Parent-child relationships for space efficiency
- **Application-Consistent Snapshots:** Integration with filesystem freeze/thaw

## Performance Characteristics

### LegacyBackend

| Operation | Typical Performance | Notes |
|:----------|:-------------------|:------|
| **Sequential Read** | ~GB/s | Filesystem cache helps |
| **Random Read** | ~MB/s | Depends on storage device |
| **Sequential Write** | ~GB/s | May suffer from write amplification on SSDs |
| **Random Write** | ~MB/s | Subject to filesystem overhead |
| **Sparse Creation** | Instant | Only allocates metadata |

### MagmaBackend (Future)

| Operation | Target Performance | Notes |
|:----------|:-------------------|:------|
| **Sequential Read** | ~GB/s | Direct device I/O |
| **Random Read** | ~GB/s | L2P map overhead minimal |
| **Sequential Write** | ~GB/s | Append-only log |
| **Random Write** | ~GB/s | Transformed to sequential |
| **Write Amplification** | ~1.0x | Near-zero (pending GC) |

## Platform Support

### Linux

✅ **Fully Supported**
- Sparse file support (ext4, xfs, btrfs)
- `io_uring` ready (Phase 8.3)
- SPDK ready (Phase 8.2)

### macOS

✅ **Fully Supported**
- Sparse file support (APFS, HFS+)
- File-based backend only

### Windows

✅ **Fully Supported**
- Sparse file support (NTFS)
- Explicit file sharing (`FILE_SHARE_READ | FILE_SHARE_WRITE`)
- Tested on Windows 10/11

## Error Handling

The Foundry uses comprehensive error types:

```rust
use foundry::error::{FoundryError, Result};

match volume.write_at(offset, data).await {
    Ok(()) => println!("Write successful"),
    Err(FoundryError::OutOfBounds { offset, len, volume_size }) => {
        eprintln!("Write out of bounds: offset={}, len={}, size={}",
                  offset, len, volume_size);
    }
    Err(FoundryError::IoError { offset, source }) => {
        eprintln!("I/O error at offset {}: {}", offset, source);
    }
    Err(FoundryError::VolumeNotFound(id)) => {
        eprintln!("Volume not found: {:?}", id);
    }
    Err(e) => eprintln!("Error: {}", e),
}
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|:---------|:--------|:------------|
| `SPACE_DATA_DIR` | Platform-specific | Volume storage directory |

**Platform Defaults:**
- Linux: `/var/lib/space/volumes`
- macOS: `/usr/local/var/space/volumes`
- Windows: `C:\ProgramData\Space\volumes`

### Feature Flags

| Flag | Status | Description |
|:-----|:-------|:------------|
| `default` | ✅ Stable | Core functionality only |
| `magma` | 🔮 Future | Enable Magma backend (requires SPDK) |
| `uring` | 🔮 Future | Enable io_uring support (Linux only) |

## Testing

### Running Tests

```bash
# Unit tests
cd crates/foundry
cargo test

# Integration tests only
cargo test --test integration

# With verbose output
cargo test -- --nocapture
```

### Test Coverage

- **28 unit tests** - Backend implementations, error handling, device abstraction
- **9 integration tests** - Volume lifecycle, concurrent access, resize, sparse operations
- **1 doc test** - Usage example verification

## Future Roadmap

### Phase 8.1: The Bridge (Snapshot Engine) ✅ COMPLETE
- ✅ Point-in-time volume snapshots
- ✅ Integration with Capsule Registry
- ✅ Deduplication via 64KB chunking
- ✅ Manifest-based snapshot metadata
- 🔮 Incremental snapshots
- 🔮 Copy-on-write optimization

### Phase 8.2: Garbage Collection
- Background compaction for Magma
- Live set tracking
- Segment cleaning algorithm
- Space reclamation

### Phase 8.3: SPDK Integration
- Replace DirectIoDevice stub with SPDK NVMe bdev
- Zero-copy DMA transfers
- Raw device access for Magma
- NVMe command passthrough

### Phase 8.4: io_uring Direct I/O
- O_DIRECT support for LegacyBackend on Linux
- Aligned buffer management
- Integration with existing io_uring transport

### Phase 8.5: Replication
- Volume-level mirroring
- Integration with PODMS scaling layer
- Cross-datacenter replication
- Consistency guarantees

## Design Decisions

### Why BoxFuture Instead of #[async_trait]?

The Foundry uses manual `BoxFuture` to match SPACE's existing `StorageBackend` trait pattern. Benefits:
- Explicit lifetime control
- No macro dependency
- Consistent with codebase style
- Better compile-time error messages

### Why Interior Mutability?

Backends use `Arc<RwLock<_>>` to enable `Arc<dyn VolumeBackend>` usage patterns. This allows:
- Thread-safe shared ownership
- Concurrent readers (via RwLock)
- Trait object compatibility
- Clean API without `&mut self` everywhere

### Why DashMap for L2P?

MagmaBackend uses DashMap for lock-free concurrent access:
- Zero lock contention on hot paths
- Predictable performance
- Proven in production (used by many Rust projects)
- Better than `RwLock<HashMap>` for concurrent workloads

## Troubleshooting

### Volume Creation Fails

**Problem:** `create_volume()` returns an error

**Solutions:**
1. Check data directory permissions
2. Ensure sufficient disk space
3. Verify filesystem supports sparse files (NTFS, ext4, xfs, btrfs, APFS)

### Concurrent Writes Corrupted

**Problem:** Data corruption with concurrent writes

**Solution:** LegacyBackend uses seek+write which is not atomic. For concurrent writers:
1. Serialize writes at application level
2. Use non-overlapping write regions
3. Wait for Phase 8.3 (io_uring with atomic positioned writes)

### Windows File Sharing Error

**Problem:** "The process cannot access the file because it is being used by another process"

**Solution:** LegacyBackend automatically sets `FILE_SHARE_READ | FILE_SHARE_WRITE`. If error persists:
1. Check for external processes locking the file
2. Ensure antivirus is not scanning volume files
3. Use unique volume IDs to avoid conflicts

### Magma Backend Unavailable

**Problem:** `BackendType::Magma` returns `BackendUnavailable` error

**Explanation:** Magma backend requires SPDK integration (Phase 8.2). Current implementation is a stub.

**Workaround:** Use `BackendType::Auto` for graceful fallback to Legacy

## Security Considerations

### Data-at-Rest Encryption

Foundry operates at the block level. For encryption:
1. Use full-disk encryption (BitLocker, LUKS, FileVault)
2. Or integrate with SPACE's encryption layer (Phase 3)
3. Future: Volume-level encryption option

### Access Control

Volumes are files in the data directory. Secure by:
1. Restricting filesystem permissions
2. Using dedicated user account for SPACE
3. SELinux/AppArmor policies for production

### Secure Deletion

To securely delete volumes:
```rust
// Delete from registry
foundry.delete_volume(volume_id).await?;

// Securely wipe file (not implemented - manual for now)
// Use tools like `shred`, `secure-delete`, or cipher.exe
```

## References

- [VolumeBackend Trait Source](../../crates/foundry/src/backend/mod.rs)
- [LegacyBackend Implementation](../../crates/foundry/src/backend/legacy.rs)
- [MagmaBackend Implementation](../../crates/foundry/src/backend/magma.rs)
- [Integration Tests](../../crates/foundry/tests/integration.rs)

## Contributing

When contributing to the Foundry:

1. **Follow BoxFuture pattern** - No #[async_trait]
2. **Add comprehensive tests** - Unit + integration
3. **Document platform differences** - Windows/Linux/macOS
4. **Benchmark performance** - Before/after for optimizations
5. **Update this guide** - Keep documentation current

## License

Apache 2.0 - See [LICENSE](../../LICENSE) for details.

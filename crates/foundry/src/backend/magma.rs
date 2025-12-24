//! Log-structured volume backend (Magma Mode).
//!
//! This backend implements a log-structured storage design that transforms
//! random writes into sequential writes, optimizing for raw NVMe performance.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures::future::BoxFuture;

use super::device::DirectIoDevice;
use super::{VolumeBackend, VolumeId};
use crate::error::{FoundryError, Result};

use serde::{Deserialize, Serialize};

/// On-disk header for each block in the log.
///
/// Layout (16 bytes, packed):
/// - magic: [u8; 4] - "MGMA" marker for validation
/// - lba: u64 - Logical block number (key in L2P map)
/// - len: u32 - Payload length in bytes
///
/// Precedes the actual data in the log:
/// [BlockHeader (16 bytes)][Data (len bytes)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BlockHeader {
    magic: [u8; 4], // "MGMA" = [0x4D, 0x47, 0x4D, 0x41]
    lba: u64,       // Logical block address
    len: u32,       // Data length
}

impl BlockHeader {
    const MAGIC: [u8; 4] = *b"MGMA";
    const SIZE: u64 = 16;

    fn new(lba: u64, len: u32) -> Self {
        Self {
            magic: Self::MAGIC,
            lba,
            len,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.magic != Self::MAGIC {
            return Err(FoundryError::device_error(format!(
                "Invalid block header magic: expected {:?}, got {:?}",
                Self::MAGIC,
                self.magic
            )));
        }
        Ok(())
    }

    fn to_bytes(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..12].copy_from_slice(&self.lba.to_le_bytes());
        buf[12..16].copy_from_slice(&self.len.to_le_bytes());
        buf
    }

    fn from_bytes(buf: &[u8]) -> Result<Self> {
        if buf.len() < 16 {
            return Err(FoundryError::device_error(
                "Buffer too small for BlockHeader",
            ));
        }

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&buf[0..4]);

        let lba = u64::from_le_bytes(buf[4..12].try_into().unwrap());
        let len = u32::from_le_bytes(buf[12..16].try_into().unwrap());

        let header = Self { magic, lba, len };
        header.validate()?;
        Ok(header)
    }
}

/// Physical address in the log-structured storage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PhysicalAddr {
    /// Physical offset in the device
    offset: u64,
    /// Length of the data at this location
    #[allow(dead_code)] // Used for future validation and GC
    len: u32,
}

/// Garbage collection statistics.
#[derive(Debug, Clone, Default)]
pub struct GcStats {
    pub blocks_moved: usize,
    pub bytes_reclaimed: u64,
    pub duration_ms: u64,
}

/// Checkpoint metadata for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MagmaCheckpoint {
    version: u32,
    volume_id: VolumeId,
    block_size: u64,
    write_head: u64,
    l2p_entries: Vec<(u64, PhysicalAddr)>,
}

impl MagmaCheckpoint {
    const VERSION: u32 = 1;

    fn new(
        volume_id: VolumeId,
        block_size: u64,
        write_head: u64,
        l2p_map: &DashMap<u64, PhysicalAddr>,
    ) -> Self {
        let l2p_entries: Vec<_> = l2p_map
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();

        Self {
            version: Self::VERSION,
            volume_id,
            block_size,
            write_head,
            l2p_entries,
        }
    }
}

/// Log-structured volume backend with L2P mapping.
///
/// ## Architecture
///
/// - **Append-Only Log**: All writes are sequential appends
/// - **L2P Map**: DashMap-based logical-to-physical address translation
/// - **Block Granularity**: Configurable block size (default 4KB)
/// - **Lock-Free**: DashMap enables concurrent reads without blocking
///
/// ## Performance Benefits
///
/// - **Write Amplification**: Near-zero (sequential writes only)
/// - **SSD Endurance**: Maximized (no in-place updates)
/// - **Concurrency**: Lock-free reads, atomic write head allocation
/// - **NVMe Optimization**: Sequential writes match device geometry
///
/// ## Future Work
///
/// - Garbage collection (Phase 8.1)
/// - SPDK integration (Phase 8.2)
/// - Snapshot support (Phase 8.4)
pub struct MagmaBackend {
    volume_id: VolumeId,
    size: u64,
    /// Logical-to-physical address map
    l2p_map: Arc<DashMap<u64, PhysicalAddr>>,
    /// Append-only write head (atomic allocation)
    write_head: Arc<AtomicU64>,
    /// Direct I/O device abstraction
    device: Arc<DirectIoDevice>,
    /// Block size for L2P granularity (default: 4KB)
    block_size: u64,
    /// Path to checkpoint file for durability
    checkpoint_path: PathBuf,
}

impl MagmaBackend {
    /// Default block size (4KB) - matches typical page size
    pub const DEFAULT_BLOCK_SIZE: u64 = 4096;

    /// Create a new Magma backend.
    ///
    /// # Arguments
    ///
    /// - `volume_id`: Unique volume identifier
    /// - `size`: Logical volume size in bytes
    /// - `device`: Direct I/O device for physical storage
    pub fn new(volume_id: VolumeId, size: u64, device: DirectIoDevice) -> Self {
        Self::with_block_size(volume_id, size, device, Self::DEFAULT_BLOCK_SIZE)
    }

    /// Create a new Magma backend with custom block size.
    pub fn with_block_size(
        volume_id: VolumeId,
        size: u64,
        device: DirectIoDevice,
        block_size: u64,
    ) -> Self {
        let checkpoint_path = Self::checkpoint_path_from_device(&device);
        Self {
            volume_id,
            size,
            l2p_map: Arc::new(DashMap::new()),
            write_head: Arc::new(AtomicU64::new(0)),
            device: Arc::new(device),
            block_size,
            checkpoint_path,
        }
    }

    /// Generate checkpoint file path from device path.
    fn checkpoint_path_from_device(device: &DirectIoDevice) -> PathBuf {
        device.path().with_extension("checkpoint")
    }

    /// Get the volume ID.
    pub fn volume_id(&self) -> VolumeId {
        self.volume_id
    }

    /// Get the block size.
    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Get the current write head position.
    pub fn write_head_position(&self) -> u64 {
        self.write_head.load(Ordering::SeqCst)
    }

    /// Get L2P map entry count (for diagnostics).
    pub fn l2p_entry_count(&self) -> usize {
        self.l2p_map.len()
    }

    /// Run garbage collection (stub).
    ///
    /// # Future Implementation (Phase 8.1)
    ///
    /// 1. Identify dead blocks (overwritten in L2P)
    /// 2. Read live data from fragmented regions
    /// 3. Rewrite compacted to new physical offset
    /// 4. Update L2P atomically
    /// 5. Reclaim space
    pub async fn gc_compact(&mut self) -> Result<GcStats> {
        // Placeholder for background GC
        tracing::debug!(volume_id = ?self.volume_id, "GC not yet implemented");
        Ok(GcStats::default())
    }

    /// Save checkpoint to disk.
    ///
    /// Creates an atomic snapshot of the L2P map and write head position.
    /// Uses atomic write-then-rename pattern for crash safety.
    pub async fn checkpoint(&self) -> Result<()> {
        let checkpoint = MagmaCheckpoint::new(
            self.volume_id,
            self.block_size,
            self.write_head.load(Ordering::SeqCst),
            &self.l2p_map,
        );

        let bytes = bincode::serialize(&checkpoint).map_err(|e| {
            FoundryError::config_error(format!("Failed to serialize checkpoint: {}", e))
        })?;

        // Atomic write: write to temp file, then rename
        let temp_path = self.checkpoint_path.with_extension("tmp");
        tokio::fs::write(&temp_path, &bytes).await?;
        tokio::fs::rename(&temp_path, &self.checkpoint_path).await?;

        tracing::info!(
            volume_id = ?self.volume_id,
            l2p_entries = self.l2p_map.len(),
            write_head = self.write_head.load(Ordering::SeqCst),
            "Checkpoint saved"
        );

        Ok(())
    }

    /// Replay the log from current write_head to EOF.
    ///
    /// Rebuilds L2P map entries for any writes that occurred after
    /// the last checkpoint.
    async fn replay_log(&self) -> Result<()> {
        let mut offset = self.write_head.load(Ordering::SeqCst);
        let mut replayed = 0;

        tracing::info!("Replaying log from offset {}", offset);

        loop {
            // Try to read header
            let header_bytes = match self
                .device
                .read_at(offset, BlockHeader::SIZE as usize)
                .await
            {
                Ok(bytes) => bytes,
                Err(_) => {
                    // EOF or corruption, stop replay
                    break;
                }
            };

            // Parse and validate header
            let header = match BlockHeader::from_bytes(&header_bytes) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "Invalid header at offset {}, stopping replay: {}",
                        offset,
                        e
                    );
                    break;
                }
            };

            // Skip to next block (header + data)
            let block_size = BlockHeader::SIZE + header.len as u64;

            // Update L2P map with this block
            self.l2p_map.insert(
                header.lba,
                PhysicalAddr {
                    offset: offset + BlockHeader::SIZE,
                    len: header.len,
                },
            );

            offset += block_size;
            replayed += 1;
        }

        // Update write_head to end of log
        self.write_head.store(offset, Ordering::SeqCst);

        tracing::info!(
            "Replay complete: {} blocks replayed, write_head={}",
            replayed,
            offset
        );

        Ok(())
    }

    /// Open existing Magma volume with recovery.
    ///
    /// # Recovery Process
    /// 1. Load checkpoint file (if exists)
    /// 2. Restore L2P map and write_head from checkpoint
    /// 3. Replay log from checkpoint.write_head to EOF
    /// 4. Update L2P map with any new writes after checkpoint
    /// 5. Set write_head to actual end of log
    pub async fn open(
        volume_id: VolumeId,
        size: u64,
        device: DirectIoDevice,
        block_size: u64,
    ) -> Result<Self> {
        let checkpoint_path = Self::checkpoint_path_from_device(&device);

        // Step 1: Try to load checkpoint
        let checkpoint = if tokio::fs::try_exists(&checkpoint_path).await? {
            tracing::info!("Loading checkpoint from {:?}", checkpoint_path);
            let bytes = tokio::fs::read(&checkpoint_path).await?;
            Some(
                bincode::deserialize::<MagmaCheckpoint>(&bytes).map_err(|e| {
                    FoundryError::config_error(format!("Failed to deserialize checkpoint: {}", e))
                })?,
            )
        } else {
            tracing::info!("No checkpoint found, starting fresh");
            None
        };

        let l2p_map = Arc::new(DashMap::new());
        let write_head = Arc::new(AtomicU64::new(0));

        // Step 2: Restore from checkpoint if available
        if let Some(ckpt) = checkpoint {
            // Validate checkpoint matches volume
            if ckpt.volume_id != volume_id {
                return Err(FoundryError::config_error(format!(
                    "Checkpoint volume_id mismatch: expected {:?}, got {:?}",
                    volume_id, ckpt.volume_id
                )));
            }
            if ckpt.block_size != block_size {
                return Err(FoundryError::config_error(format!(
                    "Checkpoint block_size mismatch: expected {}, got {}",
                    block_size, ckpt.block_size
                )));
            }

            // Restore L2P map
            for (lba, phys_addr) in ckpt.l2p_entries {
                l2p_map.insert(lba, phys_addr);
            }

            write_head.store(ckpt.write_head, Ordering::SeqCst);

            tracing::info!(
                "Restored checkpoint: {} L2P entries, write_head={}",
                l2p_map.len(),
                ckpt.write_head
            );
        }

        let backend = Self {
            volume_id,
            size,
            l2p_map,
            write_head,
            device: Arc::new(device),
            block_size,
            checkpoint_path,
        };

        // Step 3: Replay log from checkpoint to EOF
        backend.replay_log().await?;

        Ok(backend)
    }

    /// Open or create Magma backend (convenience method).
    ///
    /// Opens existing volume if checkpoint exists, otherwise creates new volume.
    pub async fn open_or_create(
        volume_id: VolumeId,
        size: u64,
        device: DirectIoDevice,
        block_size: u64,
    ) -> Result<Self> {
        let checkpoint_path = Self::checkpoint_path_from_device(&device);

        if tokio::fs::try_exists(&checkpoint_path).await? {
            Self::open(volume_id, size, device, block_size).await
        } else {
            Ok(Self::with_block_size(volume_id, size, device, block_size))
        }
    }
}

impl VolumeBackend for MagmaBackend {
    fn init(&self, size_bytes: u64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Validate size matches
            if size_bytes != self.size {
                return Err(FoundryError::config_error(format!(
                    "Size mismatch: expected {}, got {}",
                    self.size, size_bytes
                )));
            }

            tracing::info!(
                volume_id = ?self.volume_id,
                size_bytes = size_bytes,
                block_size = self.block_size,
                device_path = ?self.device.path(),
                "Initialized Magma backend"
            );

            Ok(())
        })
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, Result<Bytes>> {
        Box::pin(async move {
            // Bounds check
            if offset + len as u64 > self.size {
                return Err(FoundryError::out_of_bounds(offset, len, self.size));
            }

            let mut result = BytesMut::with_capacity(len);
            let mut current_offset = offset;
            let end_offset = offset + len as u64;

            // Read block by block
            while current_offset < end_offset {
                let block = current_offset / self.block_size;
                let block_start = block * self.block_size;
                let block_end = block_start + self.block_size;

                // Calculate read range within this block
                let read_start = current_offset;
                let read_end = end_offset.min(block_end);
                let read_len = (read_end - read_start) as usize;

                // Lookup physical address in L2P map
                match self.l2p_map.get(&block) {
                    Some(phys_addr) => {
                        // Read from physical location
                        let offset_in_block = (read_start - block_start) as usize;
                        let physical_offset = phys_addr.offset + offset_in_block as u64;

                        let data = self
                            .device
                            .read_at(physical_offset, read_len)
                            .await
                            .map_err(|e| {
                                FoundryError::device_error(format!(
                                    "Failed to read from device at offset {}: {}",
                                    physical_offset, e
                                ))
                            })?;

                        result.extend_from_slice(&data);
                    }
                    None => {
                        // Unwritten block - return zeros (sparse)
                        result.extend_from_slice(&vec![0u8; read_len]);
                    }
                }

                current_offset = read_end;
            }

            Ok(result.freeze())
        })
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let len = data.len();

            // Bounds check
            if offset + len as u64 > self.size {
                return Err(FoundryError::out_of_bounds(offset, len, self.size));
            }

            // Calculate block ranges
            let block_start = offset / self.block_size;
            let block_end = (offset + len as u64).div_ceil(self.block_size);

            for block in block_start..block_end {
                let block_offset = block * self.block_size;
                let data_start = offset.max(block_offset);
                let data_end = (offset + len as u64).min((block + 1) * self.block_size);

                let offset_in_data = (data_start - offset) as usize;
                let block_len = (data_end - data_start) as u32;
                let block_data = data.slice(offset_in_data..(offset_in_data + block_len as usize));

                // Allocate space for header + data
                let total_size = BlockHeader::SIZE + block_len as u64;
                let phys_offset = self.write_head.fetch_add(total_size, Ordering::SeqCst);

                // Write header
                let header = BlockHeader::new(block, block_len);
                self.device
                    .write_at(phys_offset, &header.to_bytes())
                    .await
                    .map_err(|e| {
                        FoundryError::device_error(format!(
                            "Failed to write header at offset {}: {}",
                            phys_offset, e
                        ))
                    })?;

                // Write data
                self.device
                    .write_at(phys_offset + BlockHeader::SIZE, &block_data)
                    .await
                    .map_err(|e| {
                        FoundryError::device_error(format!(
                            "Failed to write data at offset {}: {}",
                            phys_offset + BlockHeader::SIZE,
                            e
                        ))
                    })?;

                // Update L2P map (offset points to data, not header)
                self.l2p_map.insert(
                    block,
                    PhysicalAddr {
                        offset: phys_offset + BlockHeader::SIZE,
                        len: block_len,
                    },
                );
            }

            Ok(())
        })
    }

    fn sync(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.device.flush().await?;
            self.checkpoint().await?;
            Ok(())
        })
    }

    fn size(&self) -> BoxFuture<'_, Result<u64>> {
        Box::pin(async move { Ok(self.size) })
    }

    // Magma backend does not support resize
    // (would require L2P map reconstruction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_backend() -> (TempDir, MagmaBackend) {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_magma.img");
        let device = DirectIoDevice::open(&device_path).await.unwrap();

        let volume_id = VolumeId::new();
        let size = 1024 * 1024; // 1MB
        let backend = MagmaBackend::new(volume_id, size, device);

        (temp_dir, backend)
    }

    #[tokio::test]
    async fn test_magma_backend_init() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();
    }

    #[tokio::test]
    async fn test_magma_backend_write_read() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Write data
        let data = Bytes::from(vec![0xAB; 4096]);
        backend.write_at(512 * 1024, data.clone()).await.unwrap();

        // Verify L2P entry was created
        let block = (512 * 1024) / MagmaBackend::DEFAULT_BLOCK_SIZE;
        assert!(backend.l2p_map.contains_key(&block));

        // Read it back
        let read_data = backend.read_at(512 * 1024, 4096).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_magma_backend_l2p_mapping() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Write creates L2P entries
        let data = Bytes::from(vec![1, 2, 3, 4]);
        backend.write_at(4096, data.clone()).await.unwrap();

        // Verify L2P entry exists for block 1 (offset 4096 / 4096)
        assert!(backend.l2p_map.contains_key(&1));

        // Verify write head advanced (header + data = 16 + 4 = 20)
        assert_eq!(backend.write_head_position(), 20);

        // Read uses L2P map
        let read_data = backend.read_at(4096, 4).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_magma_backend_sparse_reads() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Write at offset 512KB
        let data = Bytes::from(vec![0xCD; 4096]);
        backend.write_at(512 * 1024, data.clone()).await.unwrap();

        // Read unwritten region (should be zeros)
        let zeros = backend.read_at(0, 4096).await.unwrap();
        assert_eq!(zeros, Bytes::from(vec![0u8; 4096]));

        // Read written region
        let read_data = backend.read_at(512 * 1024, 4096).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_magma_backend_sequential_writes() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Multiple writes should allocate sequentially
        let data1 = Bytes::from(vec![1; 100]);
        let data2 = Bytes::from(vec![2; 200]);
        let data3 = Bytes::from(vec![3; 300]);

        backend.write_at(0, data1).await.unwrap();
        assert_eq!(backend.write_head_position(), 116); // 16 + 100

        backend.write_at(4096, data2).await.unwrap();
        assert_eq!(backend.write_head_position(), 332); // 116 + 16 + 200

        backend.write_at(8192, data3).await.unwrap();
        assert_eq!(backend.write_head_position(), 648); // 332 + 16 + 300
    }

    #[tokio::test]
    async fn test_magma_backend_overwrite() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Write initial data
        let data1 = Bytes::from(vec![0xAA; 4096]);
        backend.write_at(0, data1).await.unwrap();

        // Overwrite with new data (creates new physical location)
        let data2 = Bytes::from(vec![0xBB; 4096]);
        backend.write_at(0, data2.clone()).await.unwrap();

        // Read should return new data
        let read_data = backend.read_at(0, 4096).await.unwrap();
        assert_eq!(read_data, data2);

        // L2P map should point to new location (first write: 16 + 4096 = 4112, second write header at 4112, data at 4112 + 16 = 4128)
        let phys_addr = backend.l2p_map.get(&0).unwrap();
        assert_eq!(phys_addr.offset, 4128); // Second write data location
    }

    #[tokio::test]
    async fn test_magma_backend_bounds_check() {
        let (_temp_dir, backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // Try to read beyond bounds
        let result = backend.read_at(1024 * 1024 - 100, 200).await;
        assert!(matches!(result, Err(FoundryError::OutOfBounds { .. })));

        // Try to write beyond bounds
        let data = Bytes::from(vec![0xFF; 200]);
        let result = backend.write_at(1024 * 1024 - 100, data).await;
        assert!(matches!(result, Err(FoundryError::OutOfBounds { .. })));
    }

    #[tokio::test]
    async fn test_magma_backend_gc_stub() {
        let (_temp_dir, mut backend) = create_test_backend().await;
        backend.init(1024 * 1024).await.unwrap();

        // GC is a stub, should return empty stats
        let stats = backend.gc_compact().await.unwrap();
        assert_eq!(stats.blocks_moved, 0);
        assert_eq!(stats.bytes_reclaimed, 0);
    }

    // ======== Milestone 8.3: Durability Tests ========

    #[tokio::test]
    async fn test_block_header_serialization() {
        let header = BlockHeader::new(42, 4096);
        let bytes = header.to_bytes();
        let parsed = BlockHeader::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.magic, BlockHeader::MAGIC);
        assert_eq!(parsed.lba, 42);
        assert_eq!(parsed.len, 4096);
    }

    #[tokio::test]
    async fn test_block_header_validation() {
        let mut bytes = BlockHeader::new(1, 100).to_bytes();
        bytes[0] = 0xFF; // Corrupt magic

        let result = BlockHeader::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_checkpoint_save_load() {
        let (_temp_dir, backend) = create_test_backend().await;

        // Write some data
        let data = Bytes::from(vec![0xAB; 4096]);
        backend.write_at(0, data).await.unwrap();

        // Save checkpoint
        backend.checkpoint().await.unwrap();

        // Verify checkpoint file exists
        assert!(tokio::fs::try_exists(&backend.checkpoint_path)
            .await
            .unwrap());

        // Load checkpoint
        let checkpoint_bytes = tokio::fs::read(&backend.checkpoint_path).await.unwrap();
        let checkpoint: MagmaCheckpoint = bincode::deserialize(&checkpoint_bytes).unwrap();

        assert_eq!(checkpoint.volume_id, backend.volume_id);
        assert_eq!(checkpoint.l2p_entries.len(), 1);
    }

    #[tokio::test]
    async fn test_checkpoint_atomic_write() {
        let (_temp_dir, backend) = create_test_backend().await;

        // Write data
        let data = Bytes::from(vec![0xDD; 4096]);
        backend.write_at(0, data).await.unwrap();

        // Checkpoint
        backend.checkpoint().await.unwrap();

        // Verify temp file doesn't exist (rename succeeded)
        let temp_path = backend.checkpoint_path.with_extension("tmp");
        assert!(!tokio::fs::try_exists(&temp_path).await.unwrap());
    }

    #[tokio::test]
    async fn test_recovery_empty_log() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_recovery.img");
        let device = DirectIoDevice::open(&device_path).await.unwrap();

        let volume_id = VolumeId::new();
        let size = 1024 * 1024;

        // Open should succeed with no checkpoint
        let backend = MagmaBackend::open(volume_id, size, device, 4096)
            .await
            .unwrap();
        assert_eq!(backend.write_head_position(), 0);
        assert_eq!(backend.l2p_entry_count(), 0);
    }

    #[tokio::test]
    async fn test_recovery_with_checkpoint() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_recovery.img");

        let volume_id = VolumeId::new();
        let size = 1024 * 1024;

        // Phase 1: Create backend, write data, checkpoint
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::new(volume_id, size, device);

            let data = Bytes::from(vec![0xAB; 4096]);
            backend.write_at(0, data.clone()).await.unwrap();
            backend.write_at(8192, data.clone()).await.unwrap();

            backend.checkpoint().await.unwrap();

            assert_eq!(backend.l2p_entry_count(), 2);
        }

        // Phase 2: Reopen and verify recovery
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::open(volume_id, size, device, 4096)
                .await
                .unwrap();

            // Verify L2P map was restored
            assert_eq!(backend.l2p_entry_count(), 2);

            // Verify data is readable
            let read_data = backend.read_at(0, 4096).await.unwrap();
            assert_eq!(read_data, Bytes::from(vec![0xAB; 4096]));
        }
    }

    #[tokio::test]
    async fn test_recovery_with_post_checkpoint_writes() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_recovery.img");

        let volume_id = VolumeId::new();
        let size = 1024 * 1024;

        // Phase 1: Write, checkpoint, write more (simulates crash after writes)
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::new(volume_id, size, device);

            // Write and checkpoint
            let data1 = Bytes::from(vec![0xAA; 4096]);
            backend.write_at(0, data1).await.unwrap();
            backend.checkpoint().await.unwrap();

            // Write more WITHOUT checkpointing (simulates crash)
            let data2 = Bytes::from(vec![0xBB; 4096]);
            backend.write_at(4096, data2).await.unwrap();
            backend.device.flush().await.unwrap();

            // Don't checkpoint - simulate crash
        }

        // Phase 2: Recovery should replay the post-checkpoint write
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::open(volume_id, size, device, 4096)
                .await
                .unwrap();

            // Should have both writes
            assert_eq!(backend.l2p_entry_count(), 2);

            // Verify both blocks are readable
            let data1 = backend.read_at(0, 4096).await.unwrap();
            assert_eq!(data1, Bytes::from(vec![0xAA; 4096]));

            let data2 = backend.read_at(4096, 4096).await.unwrap();
            assert_eq!(data2, Bytes::from(vec![0xBB; 4096]));
        }
    }

    #[tokio::test]
    async fn test_recovery_with_corrupted_header() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_recovery.img");

        let volume_id = VolumeId::new();
        let size = 1024 * 1024;

        // Phase 1: Write valid data
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::new(volume_id, size, device);

            let data = Bytes::from(vec![0xCC; 4096]);
            backend.write_at(0, data).await.unwrap();
            backend.checkpoint().await.unwrap();
        }

        // Phase 2: Corrupt the log after checkpoint
        {
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&device_path)
                .await
                .unwrap();

            // Corrupt header of potential next block
            let corrupt_offset = BlockHeader::SIZE + 4096;
            file.seek(tokio::io::SeekFrom::Start(corrupt_offset))
                .await
                .unwrap();
            file.write_all(b"XXXX").await.unwrap();
        }

        // Phase 3: Recovery should stop at corruption
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::open(volume_id, size, device, 4096)
                .await
                .unwrap();

            // Should only have the checkpointed block
            assert_eq!(backend.l2p_entry_count(), 1);
        }
    }

    #[tokio::test]
    async fn test_sync_creates_checkpoint() {
        let (_temp_dir, backend) = create_test_backend().await;

        // Write data
        let data = Bytes::from(vec![0xEE; 4096]);
        backend.write_at(0, data).await.unwrap();

        // Sync should create checkpoint
        backend.sync().await.unwrap();

        // Verify checkpoint exists
        assert!(tokio::fs::try_exists(&backend.checkpoint_path)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_open_or_create() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_open_or_create.img");

        let volume_id = VolumeId::new();
        let size = 1024 * 1024;

        // First call should create new backend
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::open_or_create(volume_id, size, device, 4096)
                .await
                .unwrap();
            assert_eq!(backend.l2p_entry_count(), 0);

            let data = Bytes::from(vec![0xFF; 4096]);
            backend.write_at(0, data).await.unwrap();
            backend.checkpoint().await.unwrap();
        }

        // Second call should open existing backend
        {
            let device = DirectIoDevice::open(&device_path).await.unwrap();
            let backend = MagmaBackend::open_or_create(volume_id, size, device, 4096)
                .await
                .unwrap();
            assert_eq!(backend.l2p_entry_count(), 1);
        }
    }
}

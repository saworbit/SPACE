//! Log-structured volume backend (Magma Mode).
//!
//! This backend implements a log-structured storage design that transforms
//! random writes into sequential writes, optimizing for raw NVMe performance.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures::future::BoxFuture;

use super::device::DirectIoDevice;
use super::{VolumeBackend, VolumeId};
use crate::error::{FoundryError, Result};

/// Physical address in the log-structured storage.
#[derive(Debug, Clone, Copy)]
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
        Self {
            volume_id,
            size,
            l2p_map: Arc::new(DashMap::new()),
            write_head: Arc::new(AtomicU64::new(0)),
            device: Arc::new(device),
            block_size,
        }
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

            // Transform random write -> sequential write
            // 1. Atomically allocate space at write head
            let phys_offset = self.write_head.fetch_add(len as u64, Ordering::SeqCst);

            // 2. Write to the log tip
            self.device
                .write_at(phys_offset, &data)
                .await
                .map_err(|e| {
                    FoundryError::device_error(format!(
                        "Failed to write to device at offset {}: {}",
                        phys_offset, e
                    ))
                })?;

            // 3. Update L2P map (block-level granularity)
            let block_start = offset / self.block_size;
            let block_end = (offset + len as u64).div_ceil(self.block_size);

            for block in block_start..block_end {
                let block_offset = block * self.block_size;
                let data_start = offset.max(block_offset);
                let data_end = (offset + len as u64).min((block + 1) * self.block_size);

                let offset_in_data = data_start - offset;
                let block_len = (data_end - data_start) as u32;

                self.l2p_map.insert(
                    block,
                    PhysicalAddr {
                        offset: phys_offset + offset_in_data,
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

        // Verify write head advanced
        assert_eq!(backend.write_head_position(), 4);

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
        assert_eq!(backend.write_head_position(), 100);

        backend.write_at(4096, data2).await.unwrap();
        assert_eq!(backend.write_head_position(), 300);

        backend.write_at(8192, data3).await.unwrap();
        assert_eq!(backend.write_head_position(), 600);
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

        // L2P map should point to new location
        let phys_addr = backend.l2p_map.get(&0).unwrap();
        assert_eq!(phys_addr.offset, 4096); // Second write location
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
}

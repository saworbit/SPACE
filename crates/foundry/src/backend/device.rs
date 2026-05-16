//! Direct I/O device abstraction for raw block storage.
//!
//! This module provides a stub implementation for direct device access.
//! Future implementations will integrate with SPDK for zero-copy DMA and
//! raw NVMe namespace access.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::error::Result;

/// Direct I/O device abstraction.
///
/// ## Current Implementation
///
/// This is a stub implementation using standard file I/O through `tokio::fs`.
/// It provides the interface needed by `MagmaBackend` while deferring actual
/// raw device integration to Phase 8.2+.
///
/// ## Future Implementation
///
/// - SPDK NVMe bdev integration
/// - Zero-copy DMA transfers
/// - io_uring with O_DIRECT
/// - NVMe command passthrough
pub struct DirectIoDevice {
    path: PathBuf,
    // TODO(foundry-direct-io): the seek-based stub serializes all reads and
    // writes under a single `RwLock<File>` because seek + read/write cannot
    // share a file cursor safely. The real backend (pwrite/pread, SPDK,
    // io_uring) must use offset-addressed I/O to allow concurrent operations —
    // do NOT copy this lock pattern into the production path.
    file: Arc<RwLock<Option<File>>>,
}

impl DirectIoDevice {
    /// Open or create a device at the specified path.
    ///
    /// In the stub implementation, this creates a regular file.
    /// Future implementations will open raw block devices.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Open or create the file
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await?;

        Ok(Self {
            path,
            file: Arc::new(RwLock::new(Some(file))),
        })
    }

    /// Create a test device (stub for testing).
    #[cfg(test)]
    pub fn stub() -> Self {
        Self {
            path: PathBuf::from("/dev/null"),
            file: Arc::new(RwLock::new(None)),
        }
    }

    /// Read data from the device at a specific offset.
    ///
    /// # Stub Implementation
    ///
    /// Uses standard file I/O with seek + read.
    ///
    /// # Future Implementation
    ///
    /// - SPDK: `spdk_bdev_read()`
    /// - io_uring: `read_at()` with O_DIRECT
    pub async fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut file_guard = self.file.write().await;
        let file = file_guard
            .as_mut()
            .ok_or_else(|| crate::error::FoundryError::device_error("Device not initialized"))?;

        // The stub implementation is seek-based, so serialize access to the
        // single file cursor. This keeps Windows read-after-write visibility
        // deterministic for tests that append and immediately read.
        file.seek(tokio::io::SeekFrom::Start(offset)).await?;
        let mut buffer = vec![0u8; len];
        file.read_exact(&mut buffer).await?;

        Ok(buffer)
    }

    /// Write data to the device at a specific offset.
    ///
    /// # Stub Implementation
    ///
    /// Uses standard file I/O with seek + write.
    ///
    /// # Future Implementation
    ///
    /// - SPDK: `spdk_bdev_write()`
    /// - io_uring: `write_at()` with O_DIRECT
    pub async fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut file_guard = self.file.write().await;
        let file = file_guard
            .as_mut()
            .ok_or_else(|| crate::error::FoundryError::device_error("Device not initialized"))?;

        file.seek(tokio::io::SeekFrom::Start(offset)).await?;
        file.write_all(data).await?;

        Ok(())
    }

    /// Flush all pending writes to the device.
    ///
    /// # Stub Implementation
    ///
    /// Uses `flush()` to ensure OS buffers are written.
    ///
    /// # Future Implementation
    ///
    /// - SPDK: `spdk_bdev_flush()`
    /// - NVMe: Direct flush command
    pub async fn flush(&self) -> Result<()> {
        let mut file_guard = self.file.write().await;
        if let Some(file) = file_guard.as_mut() {
            file.flush().await?;
        }
        Ok(())
    }

    /// Get the device path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_device_open() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_device.img");

        let device = DirectIoDevice::open(&device_path).await.unwrap();
        assert_eq!(device.path(), device_path.as_path());
    }

    #[tokio::test]
    async fn test_device_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_device.img");

        let device = DirectIoDevice::open(&device_path).await.unwrap();

        // Write data at offset 1024
        let data = b"Hello, Foundry!";
        device.write_at(1024, data).await.unwrap();

        // Read it back
        let read_data = device.read_at(1024, data.len()).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_device_flush() {
        let temp_dir = TempDir::new().unwrap();
        let device_path = temp_dir.path().join("test_device.img");

        let device = DirectIoDevice::open(&device_path).await.unwrap();

        device.write_at(0, b"test").await.unwrap();
        device.flush().await.unwrap();
    }
}

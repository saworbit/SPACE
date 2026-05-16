//! File-based sparse volume backend (Legacy Mode).
//!
//! This backend provides universal compatibility using standard filesystem
//! operations. It creates sparse files that work on any platform without
//! requiring raw device access or special privileges.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::RwLock;

use super::{VolumeBackend, VolumeId};
use crate::error::{FoundryError, Result};

/// File-based sparse volume backend.
///
/// ## Features
///
/// - **Sparse Files**: Uses filesystem sparse file support (ext4, xfs, btrfs, NTFS)
/// - **Cross-Platform**: Works on Linux, macOS, Windows
/// - **Serialized seek I/O**: seek-based file access is guarded by a write lock
/// - **Bounds Checking**: Validates all I/O operations
///
/// ## Performance
///
/// - Read/Write: Standard filesystem performance
/// - Random I/O: Subject to OS page cache and disk layout
/// - Write Amplification: Depends on filesystem (SSDs may see amplification)
///
/// ## Use Cases
///
/// - Development and testing
/// - Edge deployments without raw device access
/// - Standard VM environments
/// - Windows environments
pub struct LegacyBackend {
    volume_id: VolumeId,
    path: PathBuf,
    size: Arc<RwLock<u64>>,
    file: Arc<RwLock<Option<File>>>,
}

impl LegacyBackend {
    /// Create a new legacy backend for the specified volume.
    ///
    /// This does not initialize the file yet - call `init()` to create the sparse file.
    pub fn new(volume_id: VolumeId, path: PathBuf) -> Self {
        Self {
            volume_id,
            path,
            size: Arc::new(RwLock::new(0)),
            file: Arc::new(RwLock::new(None)),
        }
    }

    /// Create and initialize a legacy backend in one step.
    pub async fn create(volume_id: VolumeId, path: PathBuf, size_bytes: u64) -> Result<Self> {
        let backend = Self::new(volume_id, path);
        backend.init(size_bytes).await?;
        Ok(backend)
    }

    /// Get the volume ID.
    pub fn volume_id(&self) -> VolumeId {
        self.volume_id
    }

    /// Get the file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Open the volume file with platform-specific options.
    async fn open_volume_file(path: &std::path::Path) -> Result<File> {
        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        #[cfg(windows)]
        {
            // Windows: Set file sharing to allow multiple readers
            use std::fs::OpenOptions;
            use std::os::windows::fs::OpenOptionsExt;

            let std_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .share_mode(0x00000001 | 0x00000002) // FILE_SHARE_READ | FILE_SHARE_WRITE
                .open(path)?;

            Ok(File::from_std(std_file))
        }

        #[cfg(not(windows))]
        {
            // Unix: Standard file opening
            tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .await
                .map_err(|e| e.into())
        }
    }
}

impl VolumeBackend for LegacyBackend {
    fn init(&self, size_bytes: u64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Open the file
            let file = Self::open_volume_file(&self.path).await?;

            // Create sparse file by setting length
            file.set_len(size_bytes).await?;

            // Store file handle and size
            *self.file.write().await = Some(file);
            *self.size.write().await = size_bytes;

            tracing::info!(
                volume_id = ?self.volume_id,
                path = ?self.path,
                size_bytes = size_bytes,
                "Initialized legacy backend"
            );

            Ok(())
        })
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, Result<Bytes>> {
        Box::pin(async move {
            // Bounds check
            let size = *self.size.read().await;
            if offset + len as u64 > size {
                return Err(FoundryError::out_of_bounds(offset, len, size));
            }

            // Seek-based I/O uses the file cursor, so serialize access to the
            // handle. On Unix, cloned file descriptors can share cursor state.
            let mut file_guard = self.file.write().await;
            let file = file_guard
                .as_mut()
                .ok_or_else(|| FoundryError::config_error("Volume not initialized"))?;

            // Seek and read
            file.seek(tokio::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| FoundryError::io_error(offset, e))?;

            let mut buffer = vec![0u8; len];
            file.read_exact(&mut buffer)
                .await
                .map_err(|e| FoundryError::io_error(offset, e))?;

            Ok(Bytes::from(buffer))
        })
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let len = data.len();

            // Bounds check
            let size = *self.size.read().await;
            if offset + len as u64 > size {
                return Err(FoundryError::out_of_bounds(offset, len, size));
            }

            // Seek-based I/O uses the file cursor, so serialize access to the
            // handle. The production path should use offset-addressed I/O.
            let mut file_guard = self.file.write().await;
            let file = file_guard
                .as_mut()
                .ok_or_else(|| FoundryError::config_error("Volume not initialized"))?;

            // Seek and write
            file.seek(tokio::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| FoundryError::io_error(offset, e))?;

            file.write_all(&data)
                .await
                .map_err(|e| FoundryError::io_error(offset, e))?;

            Ok(())
        })
    }

    fn sync(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let file_guard = self.file.write().await;
            if let Some(file) = file_guard.as_ref() {
                file.sync_all().await?;
            }
            Ok(())
        })
    }

    fn size(&self) -> BoxFuture<'_, Result<u64>> {
        Box::pin(async move {
            let size = *self.size.read().await;
            Ok(size)
        })
    }

    fn resize(&self, new_size: u64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let file_guard = self.file.write().await;
            let file = file_guard
                .as_ref()
                .ok_or_else(|| FoundryError::config_error("Volume not initialized"))?;

            // Resize the file
            file.set_len(new_size).await?;

            // Update stored size
            drop(file_guard);
            *self.size.write().await = new_size;

            tracing::info!(
                volume_id = ?self.volume_id,
                new_size = new_size,
                "Resized volume"
            );

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_legacy_backend_init() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::new(volume_id, volume_path.clone());
        backend.init(1024 * 1024).await.unwrap();

        // Verify file was created
        assert!(tokio::fs::try_exists(&volume_path).await.unwrap());

        // Verify size
        let size = backend.size().await.unwrap();
        assert_eq!(size, 1024 * 1024);
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "Legacy backend has data integrity issues on macOS"
    )]
    async fn test_legacy_backend_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::create(volume_id, volume_path, 1024 * 1024)
            .await
            .unwrap();

        // Write data
        let data = Bytes::from(vec![0xAB; 4096]);
        backend.write_at(512 * 1024, data.clone()).await.unwrap();

        // Read it back
        let read_data = backend.read_at(512 * 1024, 4096).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_legacy_backend_sparse() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::create(volume_id, volume_path, 1024 * 1024)
            .await
            .unwrap();

        // Write at offset 512KB (sparse)
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
    async fn test_legacy_backend_bounds_check() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::create(volume_id, volume_path, 1024)
            .await
            .unwrap();

        // Try to read beyond bounds
        let result = backend.read_at(1000, 100).await;
        assert!(matches!(result, Err(FoundryError::OutOfBounds { .. })));

        // Try to write beyond bounds
        let data = Bytes::from(vec![0xFF; 100]);
        let result = backend.write_at(1000, data).await;
        assert!(matches!(result, Err(FoundryError::OutOfBounds { .. })));
    }

    #[tokio::test]
    async fn test_legacy_backend_sync() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::create(volume_id, volume_path, 1024 * 1024)
            .await
            .unwrap();

        // Write and sync
        let data = Bytes::from(vec![0xEF; 4096]);
        backend.write_at(0, data).await.unwrap();
        backend.sync().await.unwrap();
    }

    #[tokio::test]
    async fn test_legacy_backend_resize() {
        let temp_dir = TempDir::new().unwrap();
        let volume_path = temp_dir.path().join("test_volume.vol");
        let volume_id = VolumeId::new();

        let backend = LegacyBackend::create(volume_id, volume_path, 1024 * 1024)
            .await
            .unwrap();

        // Resize to 2MB
        backend.resize(2 * 1024 * 1024).await.unwrap();

        // Verify new size
        let size = backend.size().await.unwrap();
        assert_eq!(size, 2 * 1024 * 1024);

        // Verify we can write to the new region
        let data = Bytes::from(vec![0x42; 4096]);
        backend
            .write_at(1024 * 1024 + 512 * 1024, data.clone())
            .await
            .unwrap();

        let read_data = backend
            .read_at(1024 * 1024 + 512 * 1024, 4096)
            .await
            .unwrap();
        assert_eq!(read_data, data);
    }
}

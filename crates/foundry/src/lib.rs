//! The Foundry: Polymorphic Block Storage
//!
//! Foundry provides high-performance mutable block storage that defaults to raw
//! NVMe performance but safely falls back to standard filesystem operations.
//!
//! ## Architecture
//!
//! The `VolumeBackend` trait completely isolates upper layers from the physics
//! of storage. Two implementations are provided:
//!
//! - **LegacyBackend**: File-based sparse volumes using standard filesystem I/O
//! - **MagmaBackend**: Log-structured storage optimized for raw NVMe devices
//!
//! ## Usage
//!
//! ```no_run
//! use foundry::{Foundry, BackendType, VolumeId};
//! use bytes::Bytes;
//!
//! # async fn example() -> foundry::error::Result<()> {
//! // Create a Foundry instance
//! let foundry = Foundry::new();
//!
//! // Create a 10MB volume with automatic backend selection
//! let volume_id = VolumeId::new();
//! let volume = foundry.create_volume(volume_id, 10 * 1024 * 1024, None).await?;
//!
//! // Write data
//! let data = Bytes::from(vec![0x42; 4096]);
//! volume.write_at(0, data.clone()).await?;
//!
//! // Read data back
//! let read_data = volume.read_at(0, 4096).await?;
//! assert_eq!(read_data, data);
//!
//! // Sync to disk
//! volume.sync().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Deployment Strategy
//!
//! - **Development/Edge**: Default to `LegacyBackend` (works everywhere)
//! - **Production (Day 1)**: `LegacyBackend` on high-performance XFS
//! - **Hyperscale (Day 100)**: `MagmaBackend` on raw NVMe
//!
//! ## Backend Selection
//!
//! The Foundry manager supports runtime backend selection:
//!
//! - `BackendType::Auto` - Try Magma, fallback to Legacy (default)
//! - `BackendType::Legacy` - Force file-based backend
//! - `BackendType::Magma` - Force log-structured backend (fail if unavailable)

pub mod backend;
pub mod error;
pub mod replication;
pub mod snapshot;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

pub use backend::legacy::LegacyBackend;
pub use backend::magma::{GcStats, MagmaBackend};
pub use backend::{VolumeBackend, VolumeId};
pub use error::{FoundryError, Result};

/// Backend type selection for volume creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendType {
    /// Automatically select backend: try Magma, fallback to Legacy
    #[default]
    Auto,
    /// Force file-based backend (guaranteed to work)
    Legacy,
    /// Force log-structured backend (fail if unavailable)
    Magma,
}

/// Foundry volume manager.
///
/// The Foundry manages a collection of volumes and handles backend selection
/// and lifecycle management.
pub struct Foundry {
    volumes: Arc<RwLock<HashMap<VolumeId, Arc<dyn VolumeBackend>>>>,
    backend_preference: BackendType,
    data_dir: PathBuf,
}

impl Foundry {
    /// Create a new Foundry instance with default configuration.
    ///
    /// Uses `SPACE_DATA_DIR` environment variable for volume storage, or
    /// defaults to platform-specific locations:
    /// - Linux: `/var/lib/space/volumes`
    /// - Windows: `C:\ProgramData\Space\volumes`
    /// - macOS: `/usr/local/var/space/volumes`
    pub fn new() -> Self {
        let data_dir = std::env::var("SPACE_DATA_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_data_dir);

        Self {
            volumes: Arc::new(RwLock::new(HashMap::new())),
            backend_preference: BackendType::Auto,
            data_dir,
        }
    }

    /// Create a Foundry instance with specific data directory.
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            volumes: Arc::new(RwLock::new(HashMap::new())),
            backend_preference: BackendType::Auto,
            data_dir: data_dir.into(),
        }
    }

    /// Set the backend preference for new volumes.
    pub fn with_backend(mut self, backend: BackendType) -> Self {
        self.backend_preference = backend;
        self
    }

    /// Get the default data directory for the current platform.
    fn default_data_dir() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            PathBuf::from(r"C:\ProgramData\Space\volumes")
        }

        #[cfg(target_os = "macos")]
        {
            PathBuf::from("/usr/local/var/space/volumes")
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            PathBuf::from("/var/lib/space/volumes")
        }
    }

    /// Create a new volume with the specified size.
    ///
    /// # Arguments
    ///
    /// - `id`: Unique volume identifier
    /// - `size_bytes`: Logical volume size
    /// - `backend_type`: Optional backend override (uses instance preference if None)
    ///
    /// # Returns
    ///
    /// An `Arc<dyn VolumeBackend>` that can be shared across threads.
    ///
    /// # Errors
    ///
    /// - `FoundryError::VolumeExists` if a volume with this ID already exists
    /// - `FoundryError::BackendUnavailable` if the requested backend is unavailable
    pub async fn create_volume(
        &self,
        id: VolumeId,
        size_bytes: u64,
        backend_type: Option<BackendType>,
    ) -> Result<Arc<dyn VolumeBackend>> {
        // Check if volume already exists
        {
            let volumes = self.volumes.read().await;
            if volumes.contains_key(&id) {
                return Err(FoundryError::VolumeExists(id));
            }
        }

        let backend_type = backend_type.unwrap_or(self.backend_preference);

        let backend: Arc<dyn VolumeBackend> = match backend_type {
            BackendType::Auto => {
                // Try Magma first (currently always unavailable - stub)
                match Self::try_create_magma(id, size_bytes, &self.data_dir).await {
                    Ok(b) => {
                        tracing::info!(volume_id = ?id, "Created Magma backend");
                        Arc::new(b)
                    }
                    Err(e) => {
                        tracing::warn!(
                            volume_id = ?id,
                            error = %e,
                            "Magma unavailable, falling back to Legacy"
                        );
                        let b = Self::create_legacy(id, size_bytes, &self.data_dir).await?;
                        Arc::new(b)
                    }
                }
            }
            BackendType::Legacy => {
                let b = Self::create_legacy(id, size_bytes, &self.data_dir).await?;
                Arc::new(b)
            }
            BackendType::Magma => {
                let b = Self::try_create_magma(id, size_bytes, &self.data_dir).await?;
                Arc::new(b)
            }
        };

        // Initialize the volume
        backend.init(size_bytes).await?;

        // Register in volume map
        self.volumes.write().await.insert(id, backend.clone());

        Ok(backend)
    }

    /// Get an existing volume by ID.
    ///
    /// # Errors
    ///
    /// Returns `FoundryError::VolumeNotFound` if the volume doesn't exist.
    pub async fn get_volume(&self, id: VolumeId) -> Result<Arc<dyn VolumeBackend>> {
        self.volumes
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(FoundryError::VolumeNotFound(id))
    }

    /// Delete a volume.
    ///
    /// This removes the volume from the registry. Cleanup of backend storage
    /// is currently not implemented (stub).
    ///
    /// # Future Work
    ///
    /// - Delete underlying files/devices
    /// - Handle volume in-use checks
    /// - Graceful shutdown of pending I/O
    pub async fn delete_volume(&self, id: VolumeId) -> Result<()> {
        self.volumes
            .write()
            .await
            .remove(&id)
            .ok_or(FoundryError::VolumeNotFound(id))?;

        // FIXME(post-1.0): Implement backend storage cleanup (segment deletion, space reclamation).
        // Currently volume metadata is removed but underlying segments remain until GC runs.
        tracing::info!(volume_id = ?id, "deleted volume metadata; backend segments will be reclaimed by GC");

        Ok(())
    }

    /// List all registered volume IDs.
    pub async fn list_volumes(&self) -> Vec<VolumeId> {
        self.volumes.read().await.keys().copied().collect()
    }

    /// Create a Legacy backend.
    async fn create_legacy(
        id: VolumeId,
        size_bytes: u64,
        data_dir: &std::path::Path,
    ) -> Result<LegacyBackend> {
        let volume_path = data_dir.join(format!("{}.vol", id.0));
        LegacyBackend::create(id, volume_path, size_bytes).await
    }

    /// Create or open a Magma backend.
    ///
    /// Uses DirectIoDevice for storage and supports crash recovery via
    /// checkpoint-based replay. Opens existing volumes if a checkpoint exists,
    /// otherwise creates a new volume.
    async fn try_create_magma(
        id: VolumeId,
        size_bytes: u64,
        data_dir: &std::path::Path,
    ) -> Result<MagmaBackend> {
        use backend::device::DirectIoDevice;

        let device_path = data_dir.join(format!("{}.magma", id.0));
        let device = DirectIoDevice::open(&device_path).await?;

        // Use open_or_create to support recovery
        MagmaBackend::open_or_create(id, size_bytes, device, MagmaBackend::DEFAULT_BLOCK_SIZE).await
    }
}

impl Default for Foundry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_foundry_create_volume() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let volume_id = VolumeId::new();
        let size = 1024 * 1024; // 1MB

        let volume = foundry
            .create_volume(volume_id, size, Some(BackendType::Legacy))
            .await
            .unwrap();

        let volume_size = volume.size().await.unwrap();
        assert_eq!(volume_size, size);
    }

    #[tokio::test]
    async fn test_foundry_get_volume() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let volume_id = VolumeId::new();
        foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Get the volume
        let volume = foundry.get_volume(volume_id).await.unwrap();
        let size = volume.size().await.unwrap();
        assert_eq!(size, 1024 * 1024);
    }

    #[tokio::test]
    async fn test_foundry_volume_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let volume_id = VolumeId::new();
        let result = foundry.get_volume(volume_id).await;

        assert!(matches!(result, Err(FoundryError::VolumeNotFound(_))));
    }

    #[tokio::test]
    async fn test_foundry_volume_exists() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let volume_id = VolumeId::new();
        foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Try to create the same volume again
        let result = foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await;

        assert!(matches!(result, Err(FoundryError::VolumeExists(_))));
    }

    #[tokio::test]
    async fn test_foundry_delete_volume() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let volume_id = VolumeId::new();
        foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Delete the volume
        foundry.delete_volume(volume_id).await.unwrap();

        // Verify it's gone
        let result = foundry.get_volume(volume_id).await;
        assert!(matches!(result, Err(FoundryError::VolumeNotFound(_))));
    }

    #[tokio::test]
    async fn test_foundry_list_volumes() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        let id1 = VolumeId::new();
        let id2 = VolumeId::new();

        foundry
            .create_volume(id1, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();
        foundry
            .create_volume(id2, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        let volumes = foundry.list_volumes().await;
        assert_eq!(volumes.len(), 2);
        assert!(volumes.contains(&id1));
        assert!(volumes.contains(&id2));
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "Magma backend not fully functional on macOS"
    )]
    async fn test_foundry_backend_auto_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        // Auto should fallback to Legacy (Magma is stub)
        let volume_id = VolumeId::new();
        let volume = foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Auto))
            .await
            .unwrap();

        // Should be able to use it
        let data = Bytes::from(vec![0x42; 4096]);
        volume.write_at(0, data.clone()).await.unwrap();
        let read_data = volume.read_at(0, 4096).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "Magma backend not fully functional on macOS"
    )]
    async fn test_foundry_backend_magma_available() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());

        // Magma backend is now available
        let volume_id = VolumeId::new();
        let backend = foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Magma))
            .await
            .unwrap();

        // Verify it's a Magma backend by writing and reading
        let test_data = Bytes::from(vec![0xAB; 4096]);
        backend.write_at(0, test_data.clone()).await.unwrap();
        let read_data = backend.read_at(0, 4096).await.unwrap();
        assert_eq!(read_data, test_data);
    }
}

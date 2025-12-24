//! Backend abstractions for the Foundry block storage system.

use bytes::Bytes;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;

pub mod device;
pub mod legacy;
pub mod magma;

/// Unique identifier for a volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VolumeId(pub Uuid);

impl VolumeId {
    /// Create a new random volume ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create a volume ID from an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
}

impl Default for VolumeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for VolumeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for VolumeId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Abstraction for block-level volume storage.
///
/// Unlike `StorageBackend` (segment-based), `VolumeBackend` provides random-access
/// block I/O suitable for virtual disks, NVMe namespaces, and raw devices.
///
/// ## Design Pattern
///
/// This trait uses manual `BoxFuture` instead of `#[async_trait]` to match the
/// codebase pattern used in `common::traits::StorageBackend`. This provides:
/// - Explicit control over future lifetimes
/// - Consistency with existing abstractions
/// - No macro dependency
///
/// ## Interior Mutability
///
/// Backends are typically used through `Arc<dyn VolumeBackend>`. Methods that
/// logically require `&mut self` (like `write_at`) use `&self` with interior
/// mutability (e.g., `Arc<RwLock<_>>`) to enable shared ownership.
pub trait VolumeBackend: Send + Sync {
    /// Initialize or open a volume with specified size.
    ///
    /// For sparse files, this reserves the logical space.
    /// For raw devices, this validates the capacity.
    fn init(&self, size_bytes: u64) -> BoxFuture<'_, Result<()>>;

    /// Read data at a specific offset.
    ///
    /// Returns exactly `len` bytes or fails if reading beyond volume bounds.
    /// Uses `Bytes` for zero-copy where possible.
    ///
    /// # Errors
    ///
    /// - `FoundryError::OutOfBounds` if `offset + len > volume_size`
    /// - `FoundryError::IoError` for I/O failures
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, Result<Bytes>>;

    /// Write data at a specific offset.
    ///
    /// Data length determines write size. Fails if writing beyond volume bounds.
    ///
    /// # Errors
    ///
    /// - `FoundryError::OutOfBounds` if `offset + data.len() > volume_size`
    /// - `FoundryError::IoError` for I/O failures
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, Result<()>>;

    /// Flush all pending writes to stable storage.
    ///
    /// Equivalent to `fsync()` for files, NVMe flush for devices.
    fn sync(&self) -> BoxFuture<'_, Result<()>>;

    /// Get current volume size in bytes.
    fn size(&self) -> BoxFuture<'_, Result<u64>>;

    /// Resize the volume (if supported).
    ///
    /// Not all backends support online resize. Returns error if unsupported.
    ///
    /// # Default Implementation
    ///
    /// The default implementation returns `FoundryError::ResizeNotSupported`.
    fn resize(&self, _new_size: u64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::error::FoundryError::ResizeNotSupported) })
    }
}

use thiserror::Error;

use crate::backend::VolumeId;

/// Foundry-related errors for block storage operations.
#[derive(Error, Debug)]
pub enum FoundryError {
    /// Volume management errors
    #[error("Volume not found: {0:?}")]
    VolumeNotFound(VolumeId),

    #[error("Volume already exists: {0:?}")]
    VolumeExists(VolumeId),

    /// Backend errors
    #[error("Invalid backend type: {0}")]
    InvalidBackend(String),

    #[error("Backend unavailable: {reason}")]
    BackendUnavailable { reason: String },

    /// I/O errors
    #[error("I/O error at offset {offset}: {source}")]
    IoError {
        offset: u64,
        #[source]
        source: std::io::Error,
    },

    #[error("Out of bounds: offset={offset}, len={len}, volume_size={volume_size}")]
    OutOfBounds {
        offset: u64,
        len: usize,
        volume_size: u64,
    },

    #[error("Device error: {0}")]
    DeviceError(String),

    /// Garbage collection errors
    #[error("GC error: {0}")]
    GcError(String),

    /// Configuration errors
    #[error("Resize not supported for this backend")]
    ResizeNotSupported,

    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Generic wrapper for anyhow errors
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convert std::io::Error to FoundryError
impl From<std::io::Error> for FoundryError {
    fn from(err: std::io::Error) -> Self {
        FoundryError::Other(err.into())
    }
}

/// Result type alias for foundry operations
pub type Result<T> = std::result::Result<T, FoundryError>;

/// Helper constructors for ergonomic error creation
impl FoundryError {
    pub fn invalid_backend(reason: impl Into<String>) -> Self {
        FoundryError::InvalidBackend(reason.into())
    }

    pub fn backend_unavailable(reason: impl Into<String>) -> Self {
        FoundryError::BackendUnavailable {
            reason: reason.into(),
        }
    }

    pub fn io_error(offset: u64, source: std::io::Error) -> Self {
        FoundryError::IoError { offset, source }
    }

    pub fn out_of_bounds(offset: u64, len: usize, volume_size: u64) -> Self {
        FoundryError::OutOfBounds {
            offset,
            len,
            volume_size,
        }
    }

    pub fn device_error(message: impl Into<String>) -> Self {
        FoundryError::DeviceError(message.into())
    }

    pub fn config_error(message: impl Into<String>) -> Self {
        FoundryError::ConfigError(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_error_display() {
        let volume_id = VolumeId(Uuid::new_v4());
        let err = FoundryError::VolumeNotFound(volume_id);
        assert!(err.to_string().contains("Volume not found"));

        let err = FoundryError::out_of_bounds(1024, 512, 1000);
        assert_eq!(
            err.to_string(),
            "Out of bounds: offset=1024, len=512, volume_size=1000"
        );
    }

    #[test]
    fn test_error_constructors() {
        let err = FoundryError::invalid_backend("not available");
        assert_eq!(err.to_string(), "Invalid backend type: not available");

        let err = FoundryError::backend_unavailable("SPDK not initialized");
        assert!(err.to_string().contains("Backend unavailable"));
    }

    #[test]
    fn test_result_alias() {
        fn returns_error() -> Result<()> {
            Err(FoundryError::ResizeNotSupported)
        }

        let result = returns_error();
        assert!(result.is_err());
    }
}

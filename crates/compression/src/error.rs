use thiserror::Error;

/// Errors produced by compression routines.
#[derive(Debug, Error)]
pub enum CompressionError {
    /// Compression skipped because the data is already high entropy.
    #[error("Entropy too high ({entropy:.2} bits/byte) for {size} bytes")]
    EntropySkip { entropy: f32, size: usize },

    /// Compression skipped because the space savings were negligible.
    #[error("Compression ineffective (ratio {ratio:.2}) for {size} bytes")]
    IneffectiveRatio { ratio: f32, size: usize },

    /// Compression failed due to an invalid policy configuration.
    #[error("Invalid compression policy: {reason}")]
    InvalidPolicy { reason: String },

    /// Compression backend produced an IO error.
    #[error("IO error while using {algorithm}: {source}")]
    Io {
        algorithm: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Codec-specific failure without an underlying IO error.
    #[error("Codec error in {algorithm}: {message}")]
    Codec {
        algorithm: &'static str,
        message: String,
    },

    /// Integrity validation failed after recompressing a segment.
    #[error("Integrity check failed for {algorithm}")]
    IntegrityFailure { algorithm: &'static str },
}

impl CompressionError {
    pub fn invalid_policy(reason: impl Into<String>) -> Self {
        CompressionError::InvalidPolicy {
            reason: reason.into(),
        }
    }

    pub fn codec(algorithm: &'static str, message: impl Into<String>) -> Self {
        CompressionError::Codec {
            algorithm,
            message: message.into(),
        }
    }

    pub fn integrity(algorithm: &'static str) -> Self {
        CompressionError::IntegrityFailure { algorithm }
    }

    pub fn io(algorithm: &'static str, source: std::io::Error) -> Self {
        CompressionError::Io { algorithm, source }
    }
}

use anyhow::Error;
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

    pub fn io(algorithm: &'static str, source: std::io::Error) -> Self {
        CompressionError::Io { algorithm, source }
    }
}

/// Deduplication failures.
#[derive(Debug, Error)]
pub enum DedupError {
    /// Multiple segments produced the same hash but different payloads.
    #[error("Hash collision detected for {hash}")]
    HashCollision { hash: String },

    /// Failed to register a new hash with the registry.
    #[error("Failed to register hash {hash}: {reason}")]
    RegistrationFailed { hash: String, reason: String },

    /// Failed to look up a hash for deduplication.
    #[error("Failed to look up hash {hash}: {reason}")]
    LookupFailed { hash: String, reason: String },
}

/// Pipeline level failures aggregating subsystem errors.
#[derive(Debug, Error)]
pub enum PipelineError {
    /// Compression failed for a given segment.
    #[error("Compression failed for segment {segment_index}: {source}")]
    Compression {
        segment_index: usize,
        #[source]
        source: CompressionError,
    },

    /// Deduplication failed.
    #[error("Deduplication failure: {source}")]
    Dedup {
        #[source]
        source: DedupError,
    },

    /// Encryption subsystem error.
    #[error("Encryption failure: {source}")]
    Encryption {
        #[from]
        source: encryption::error::EncryptionError,
    },

    /// Capsule registry operation error.
    #[error("Registry operation `{operation}` failed: {source}")]
    Registry {
        operation: &'static str,
        #[source]
        source: Error,
    },

    /// NVRAM log operation error.
    #[error("NVRAM operation `{operation}` failed: {source}")]
    Nvram {
        operation: &'static str,
        #[source]
        source: Error,
    },

    /// Telemetry dispatch failure.
    #[error("Telemetry dispatch failed: {0}")]
    Telemetry(String),

    /// Invariants violated within the pipeline state machine.
    #[error("Pipeline invariant violated: {0}")]
    Invariant(String),
}

pub type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_error_messages() {
        let entropy = CompressionError::EntropySkip {
            entropy: 7.8,
            size: 4096,
        };
        assert!(entropy
            .to_string()
            .contains("Entropy too high (7.80 bits/byte)"));

        let ineffective = CompressionError::IneffectiveRatio {
            ratio: 1.01,
            size: 8192,
        };
        assert!(ineffective
            .to_string()
            .contains("Compression ineffective (ratio 1.01)"));
    }

    #[test]
    fn pipeline_error_wraps_compression() {
        let source = CompressionError::EntropySkip {
            entropy: 7.7,
            size: 1024,
        };

        let err = PipelineError::Compression {
            segment_index: 3,
            source,
        };

        let msg = err.to_string();
        assert!(msg.contains("Compression failed for segment 3"));
        assert!(msg.contains("Entropy too high"));
    }
}

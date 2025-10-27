use thiserror::Error;

/// Encryption-related errors
/// 
/// These errors are designed to be informative for debugging while
/// avoiding leaking sensitive information in production logs.
#[derive(Error, Debug)]
pub enum EncryptionError {
    /// Key management errors
    #[error("Key not found: version {version}")]
    KeyNotFound { version: u32 },

    #[error("Invalid key length: expected {expected}, got {actual}")]
    InvalidKeyLength { expected: usize, actual: usize },

    #[error("Key derivation failed: {0}")]
    KeyDerivationFailed(String),

    #[error("Key rotation in progress")]
    KeyRotationInProgress,

    /// Encryption/Decryption errors
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("Invalid tweak length: expected 16 bytes, got {0}")]
    InvalidTweakLength(usize),

    #[error("Invalid ciphertext length: {0}")]
    InvalidCiphertextLength(usize),

    /// Integrity errors
    #[error("Integrity verification failed: MAC mismatch")]
    IntegrityFailure,

    #[error("Missing integrity tag")]
    MissingIntegrityTag,

    #[error("Invalid MAC length: expected 16 bytes, got {0}")]
    InvalidMacLength(usize),

    /// Metadata errors
    #[error("Unsupported encryption version: {0}")]
    UnsupportedVersion(u16),

    #[error("Missing encryption metadata")]
    MissingMetadata,

    #[error("Corrupted metadata: {0}")]
    CorruptedMetadata(String),

    /// Configuration errors
    #[error("Encryption not enabled in policy")]
    EncryptionNotEnabled,

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Hardware errors
    #[error("AES-NI not available, hardware acceleration required")]
    AesNiNotAvailable,

    #[error("Hardware acceleration failed: {0}")]
    HardwareAccelerationFailed(String),

    /// Serialization errors
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Wrapped errors from dependencies
    #[error("Cipher error: {0}")]
    CipherError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Generic wrapper for anyhow errors
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result type alias for encryption operations
pub type Result<T> = std::result::Result<T, EncryptionError>;

/// Convert cipher errors to our error type
impl From<cipher::StreamCipherError> for EncryptionError {
    fn from(err: cipher::StreamCipherError) -> Self {
        EncryptionError::CipherError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = EncryptionError::KeyNotFound { version: 42 };
        assert_eq!(err.to_string(), "Key not found: version 42");

        let err = EncryptionError::IntegrityFailure;
        assert_eq!(err.to_string(), "Integrity verification failed: MAC mismatch");
    }

    #[test]
    fn test_error_from_serde() {
        let json_err = serde_json::from_str::<u32>("invalid").unwrap_err();
        let enc_err: EncryptionError = json_err.into();
        assert!(matches!(enc_err, EncryptionError::SerializationError(_)));
    }

    #[test]
    fn test_result_alias() {
        fn returns_error() -> Result<()> {
            Err(EncryptionError::EncryptionNotEnabled)
        }

        let result = returns_error();
        assert!(result.is_err());
    }
}
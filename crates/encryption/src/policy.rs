use serde::{Deserialize, Serialize};

/// Encryption algorithm selection
///
/// Each variant represents a different encryption approach, with distinct
/// security properties and performance characteristics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EncryptionPolicy {
    /// No encryption (default for backward compatibility)
    ///
    /// Use this for:
    /// - Migration from Phase 2.x
    /// - Public/non-sensitive data
    /// - Performance-critical paths where encryption is not required
    None,

    /// XTS-AES-256 with Poly1305 MAC (Phase 3.1 MVP)
    ///
    /// Properties:
    /// - 512-bit keys (two AES-256 keys for XTS mode)
    /// - Deterministic tweaks derived from content hash
    /// - Preserves deduplication over encrypted data
    /// - Poly1305 MAC for integrity verification
    ///
    /// Use this for:
    /// - At-rest encryption with dedup preservation
    /// - Standard enterprise security requirements
    /// - Hardware-accelerated environments (AES-NI)
    XtsAes256 {
        /// Key version to use for encryption
        key_version: u32,
    },

    /// ChaCha20-Poly1305 with convergent encryption (Phase 3.2)
    ///
    /// Future: AEAD with built-in authentication, convergent key derivation
    #[cfg(feature = "experimental")]
    ChaCha20Poly1305 { key_version: u32 },

    /// Message-Locked Encryption with Post-Quantum wrapping (Phase 3.3)
    ///
    /// Future: Convergent encryption + ML-KEM (Kyber) for quantum resistance
    #[cfg(feature = "experimental")]
    MlePqc { key_version: u32, pqc_enabled: bool },
}

impl Default for EncryptionPolicy {
    fn default() -> Self {
        // Default to None for backward compatibility with Phase 2.x
        EncryptionPolicy::None
    }
}

impl EncryptionPolicy {
    /// Check if encryption is enabled
    pub fn is_enabled(&self) -> bool {
        !matches!(self, EncryptionPolicy::None)
    }

    /// Get the key version (if applicable)
    pub fn key_version(&self) -> Option<u32> {
        match self {
            EncryptionPolicy::None => None,
            EncryptionPolicy::XtsAes256 { key_version } => Some(*key_version),
            #[cfg(feature = "experimental")]
            EncryptionPolicy::ChaCha20Poly1305 { key_version } => Some(*key_version),
            #[cfg(feature = "experimental")]
            EncryptionPolicy::MlePqc { key_version, .. } => Some(*key_version),
        }
    }

    /// Get encryption algorithm name (for logging/metrics)
    pub fn algorithm_name(&self) -> &str {
        match self {
            EncryptionPolicy::None => "none",
            EncryptionPolicy::XtsAes256 { .. } => "xts-aes-256",
            #[cfg(feature = "experimental")]
            EncryptionPolicy::ChaCha20Poly1305 { .. } => "chacha20-poly1305",
            #[cfg(feature = "experimental")]
            EncryptionPolicy::MlePqc { .. } => "mle-pqc",
        }
    }
}

/// Per-segment encryption metadata
///
/// These fields are optional to maintain backward compatibility with
/// unencrypted segments from Phase 2.x. All encrypted segments must
/// populate these fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EncryptionMetadata {
    /// Encryption format version (for future migrations)
    ///
    /// Version mapping:
    /// - None: Unencrypted segment
    /// - 1: XTS-AES-256 with Poly1305
    /// - 2+: Future formats
    pub encryption_version: Option<u16>,

    /// Key version used for this segment
    ///
    /// Enables key rotation without re-encrypting all data immediately.
    /// Old segments remain readable with old keys until re-encrypted.
    pub key_version: Option<u32>,

    /// Tweak/nonce for XTS mode (16 bytes)
    ///
    /// Derived deterministically from content hash:
    /// `tweak = BLAKE3(content_hash)[0..16]`
    ///
    /// This ensures identical plaintext → identical ciphertext,
    /// which preserves deduplication.
    pub tweak_nonce: Option<[u8; 16]>,

    /// Poly1305 integrity tag (16 bytes)
    ///
    /// Computed over: ciphertext || serialized_metadata
    /// Verified on read to detect tampering or corruption.
    pub integrity_tag: Option<[u8; 16]>,

    /// Length of ciphertext in bytes
    ///
    /// May differ from plaintext length due to block padding.
    /// Used for validation and offset calculations.
    pub ciphertext_len: Option<u32>,
}

impl EncryptionMetadata {
    /// Create new metadata for XTS encryption
    pub fn new_xts(key_version: u32, tweak: [u8; 16], ciphertext_len: u32) -> Self {
        Self {
            encryption_version: Some(1), // Version 1 = XTS-AES-256
            key_version: Some(key_version),
            tweak_nonce: Some(tweak),
            integrity_tag: None, // Set after MAC computation
            ciphertext_len: Some(ciphertext_len),
        }
    }

    /// Create unencrypted metadata (all None)
    pub fn new_unencrypted() -> Self {
        Self::default()
    }

    /// Check if segment is encrypted
    pub fn is_encrypted(&self) -> bool {
        self.encryption_version.is_some()
    }

    /// Verify integrity tag is present
    pub fn has_integrity_tag(&self) -> bool {
        self.integrity_tag.is_some()
    }

    /// Set the integrity tag (called after MAC computation)
    pub fn set_integrity_tag(&mut self, tag: [u8; 16]) {
        self.integrity_tag = Some(tag);
    }

    /// Get the encryption version or error if not encrypted
    pub fn require_version(&self) -> Result<u16, &'static str> {
        self.encryption_version.ok_or("Segment is not encrypted")
    }

    /// Get the key version or error if not encrypted
    pub fn require_key_version(&self) -> Result<u32, &'static str> {
        self.key_version.ok_or("Segment is not encrypted")
    }

    /// Get the tweak or error if not present
    pub fn require_tweak(&self) -> Result<[u8; 16], &'static str> {
        self.tweak_nonce.ok_or("Missing tweak nonce")
    }

    /// Get the integrity tag or error if not present
    pub fn require_integrity_tag(&self) -> Result<[u8; 16], &'static str> {
        self.integrity_tag.ok_or("Missing integrity tag")
    }
}

/// Encryption statistics for monitoring
///
/// Tracks encryption ratio, key versions in use, and total bytes encrypted.
/// Used for observability and compliance reporting.
#[derive(Debug, Clone, Default)]
pub struct EncryptionStats {
    pub encrypted_segments: usize,
    pub unencrypted_segments: usize,
    pub total_ciphertext_bytes: u64,
    pub key_versions_used: std::collections::HashSet<u32>,
}

impl EncryptionStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an encrypted segment
    pub fn add_encrypted(&mut self, key_version: u32, bytes: u64) {
        self.encrypted_segments += 1;
        self.total_ciphertext_bytes += bytes;
        self.key_versions_used.insert(key_version);
    }

    /// Record an unencrypted segment
    pub fn add_unencrypted(&mut self) {
        self.unencrypted_segments += 1;
    }

    /// Calculate encryption ratio (0.0 to 1.0)
    pub fn encryption_ratio(&self) -> f32 {
        let total = self.encrypted_segments + self.unencrypted_segments;
        if total == 0 {
            return 0.0;
        }
        self.encrypted_segments as f32 / total as f32
    }

    /// Get total segments processed
    pub fn total_segments(&self) -> usize {
        self.encrypted_segments + self.unencrypted_segments
    }

    /// Check if any encryption has occurred
    pub fn has_encrypted_data(&self) -> bool {
        self.encrypted_segments > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = EncryptionPolicy::default();
        assert_eq!(policy, EncryptionPolicy::None);
        assert!(!policy.is_enabled());
        assert_eq!(policy.algorithm_name(), "none");
    }

    #[test]
    fn test_xts_policy() {
        let policy = EncryptionPolicy::XtsAes256 { key_version: 42 };
        assert!(policy.is_enabled());
        assert_eq!(policy.key_version(), Some(42));
        assert_eq!(policy.algorithm_name(), "xts-aes-256");
    }

    #[test]
    fn test_metadata_creation() {
        let tweak = [0u8; 16];
        let meta = EncryptionMetadata::new_xts(1, tweak, 4096);

        assert!(meta.is_encrypted());
        assert_eq!(meta.encryption_version, Some(1));
        assert_eq!(meta.key_version, Some(1));
        assert_eq!(meta.ciphertext_len, Some(4096));
        assert!(!meta.has_integrity_tag());
    }

    #[test]
    fn test_unencrypted_metadata() {
        let meta = EncryptionMetadata::new_unencrypted();
        assert!(!meta.is_encrypted());
        assert!(!meta.has_integrity_tag());
    }

    #[test]
    fn test_metadata_setters() {
        let mut meta = EncryptionMetadata::new_xts(1, [0u8; 16], 4096);
        assert!(!meta.has_integrity_tag());

        meta.set_integrity_tag([42u8; 16]);
        assert!(meta.has_integrity_tag());
        assert_eq!(meta.integrity_tag, Some([42u8; 16]));
    }

    #[test]
    fn test_metadata_require_methods() {
        let meta = EncryptionMetadata::new_xts(5, [1u8; 16], 1024);

        assert_eq!(meta.require_version().unwrap(), 1);
        assert_eq!(meta.require_key_version().unwrap(), 5);
        assert_eq!(meta.require_tweak().unwrap(), [1u8; 16]);

        // Should error when no tag set
        assert!(meta.require_integrity_tag().is_err());
    }

    #[test]
    fn test_encryption_stats() {
        let mut stats = EncryptionStats::new();

        stats.add_encrypted(1, 1024);
        stats.add_encrypted(1, 2048);
        stats.add_encrypted(2, 4096);
        stats.add_unencrypted();

        assert_eq!(stats.encrypted_segments, 3);
        assert_eq!(stats.unencrypted_segments, 1);
        assert_eq!(stats.total_ciphertext_bytes, 7168);
        assert_eq!(stats.total_segments(), 4);
        assert!(stats.has_encrypted_data());

        let ratio = stats.encryption_ratio();
        assert!((ratio - 0.75).abs() < 0.01); // 75%

        assert_eq!(stats.key_versions_used.len(), 2);
        assert!(stats.key_versions_used.contains(&1));
        assert!(stats.key_versions_used.contains(&2));
    }

    #[test]
    fn test_serialization() {
        let policy = EncryptionPolicy::XtsAes256 { key_version: 42 };
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: EncryptionPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, deserialized);

        let meta = EncryptionMetadata::new_xts(1, [5u8; 16], 2048);
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: EncryptionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, deserialized);
    }
}

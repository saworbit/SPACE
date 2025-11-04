//! Key Management System
//!
//! Handles encryption key lifecycle: generation, storage, versioning, and rotation.
//! Keys are derived from a master key and versioned for rotation without data re-encryption.
//!
//! ## Security Model
//!
//! - Master key stored securely (env var, TPM, or KMS in future)
//! - Per-version keys derived using BLAKE3 KDF
//! - XTS requires 512-bit keys (two AES-256 keys)
//! - Keys are zeroized on drop
//!
//! ## Key Versioning
//!
//! Version 1 → Keys derived from master_key || version
//! Version 2 → New derivation when rotated
//! Old versions kept for reading legacy segments

use crate::error::{EncryptionError, Result};
use blake3;
use std::collections::HashMap;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// XTS-AES-256 requires 512 bits (64 bytes) - two AES-256 keys
pub const XTS_KEY_SIZE: usize = 64;

/// Master key size (256 bits)
pub const MASTER_KEY_SIZE: usize = 32;

/// Key derivation context string
const KDF_CONTEXT: &[u8] = b"SPACE-XTS-AES-256-KEY-V1";

/// A single XTS key pair (512 bits total)
///
/// Zeroized on drop for security
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct XtsKeyPair {
    /// First AES-256 key (32 bytes)
    key1: [u8; 32],
    /// Second AES-256 key (32 bytes)
    key2: [u8; 32],
}

impl XtsKeyPair {
    /// Create from 64-byte array
    pub fn from_bytes(bytes: [u8; XTS_KEY_SIZE]) -> Self {
        let mut key1 = [0u8; 32];
        let mut key2 = [0u8; 32];
        key1.copy_from_slice(&bytes[0..32]);
        key2.copy_from_slice(&bytes[32..64]);

        Self { key1, key2 }
    }

    /// Get first key (for XTS encryption/decryption)
    pub fn key1(&self) -> &[u8; 32] {
        &self.key1
    }

    /// Get second key (for XTS tweak encryption)
    pub fn key2(&self) -> &[u8; 32] {
        &self.key2
    }

    /// Convert to 64-byte array (for testing)
    #[cfg(test)]
    pub fn to_bytes(&self) -> [u8; XTS_KEY_SIZE] {
        let mut bytes = [0u8; XTS_KEY_SIZE];
        bytes[0..32].copy_from_slice(&self.key1);
        bytes[32..64].copy_from_slice(&self.key2);
        bytes
    }
}

impl std::fmt::Debug for XtsKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XtsKeyPair")
            .field("key1", &"[REDACTED]")
            .field("key2", &"[REDACTED]")
            .finish()
    }
}

/// Key Manager
///
/// Manages versioned encryption keys with support for rotation.
/// Keys are derived from a master key using BLAKE3 as a KDF.
pub struct KeyManager {
    /// Master key (zeroized on drop)
    master_key: [u8; MASTER_KEY_SIZE],

    /// Cached derived keys by version
    /// In production, this would be encrypted at rest or stored in HSM
    key_cache: HashMap<u32, XtsKeyPair>,

    /// Current active key version
    current_version: u32,

    /// Flag indicating if rotation is in progress
    rotating: bool,
}

impl KeyManager {
    /// Create new KeyManager with a master key
    ///
    /// # Arguments
    /// * `master_key` - 32-byte master key (from env, TPM, or KMS)
    ///
    /// # Security
    /// The master key should be:
    /// - Generated with a CSPRNG
    /// - Stored securely (TPM, KMS, or encrypted file)
    /// - Never logged or displayed
    pub fn new(master_key: [u8; MASTER_KEY_SIZE]) -> Self {
        let mut manager = Self {
            master_key,
            key_cache: HashMap::new(),
            current_version: 1,
            rotating: false,
        };

        // Pre-derive version 1 key
        if let Ok(key) = manager.derive_key(1) {
            manager.key_cache.insert(1, key);
        }

        manager
    }

    /// Create from environment variable
    ///
    /// Reads master key from SPACE_MASTER_KEY env var (hex-encoded)
    ///
    /// # Errors
    /// Returns error if env var missing or invalid
    pub fn from_env() -> Result<Self> {
        let hex_key = std::env::var("SPACE_MASTER_KEY").map_err(|_| {
            EncryptionError::InvalidConfiguration(
                "SPACE_MASTER_KEY environment variable not set".to_string(),
            )
        })?;

        let bytes = hex::decode(&hex_key).map_err(|e| {
            EncryptionError::InvalidConfiguration(format!("Invalid hex in SPACE_MASTER_KEY: {}", e))
        })?;

        if bytes.len() != MASTER_KEY_SIZE {
            return Err(EncryptionError::InvalidKeyLength {
                expected: MASTER_KEY_SIZE,
                actual: bytes.len(),
            });
        }

        let mut master_key = [0u8; MASTER_KEY_SIZE];
        master_key.copy_from_slice(&bytes);

        Ok(Self::new(master_key))
    }

    /// Generate a random master key (for testing or initialization)
    ///
    /// # Security
    /// Uses getrandom for cryptographically secure randomness
    #[cfg(test)]
    pub fn generate() -> Result<Self> {
        use rand::Rng;
        let mut rng = rand::rng();
        let mut master_key = [0u8; MASTER_KEY_SIZE];
        rng.fill(&mut master_key);
        Ok(Self::new(master_key))
    }

    /// Derive a key for a specific version using BLAKE3 as KDF
    ///
    /// Derivation: BLAKE3(master_key || context || version)
    fn derive_key(&self, version: u32) -> Result<XtsKeyPair> {
        let mut hasher = blake3::Hasher::new();

        // Input: master_key || context || version
        hasher.update(&self.master_key);
        hasher.update(KDF_CONTEXT);
        hasher.update(&version.to_le_bytes());

        // Derive 64 bytes for XTS (two AES-256 keys)
        let mut output = [0u8; XTS_KEY_SIZE];
        let mut output_reader = hasher.finalize_xof();
        output_reader.fill(&mut output);

        Ok(XtsKeyPair::from_bytes(output))
    }

    /// Get key for a specific version
    ///
    /// Returns cached key if available, otherwise derives and caches it
    pub fn get_key(&mut self, version: u32) -> Result<&XtsKeyPair> {
        // Check cache first
        if !self.key_cache.contains_key(&version) {
            // Derive and cache
            let key = self.derive_key(version)?;
            self.key_cache.insert(version, key);
        }

        self.key_cache
            .get(&version)
            .ok_or(EncryptionError::KeyNotFound { version })
    }

    /// Get current active key version
    pub fn current_version(&self) -> u32 {
        self.current_version
    }

    /// Check if rotation is in progress
    pub fn is_rotating(&self) -> bool {
        self.rotating
    }

    /// Begin key rotation to a new version
    ///
    /// Creates new key version and marks rotation in progress.
    /// Old keys remain available for reading legacy segments.
    pub fn rotate(&mut self) -> Result<u32> {
        if self.rotating {
            return Err(EncryptionError::KeyRotationInProgress);
        }

        self.rotating = true;
        self.current_version += 1;

        // Pre-derive new key
        let new_key = self.derive_key(self.current_version)?;
        self.key_cache.insert(self.current_version, new_key);

        Ok(self.current_version)
    }

    /// Complete key rotation
    ///
    /// Marks rotation as complete. In production, this would:
    /// - Verify all critical segments re-encrypted
    /// - Update metadata
    /// - Optionally purge old keys
    pub fn complete_rotation(&mut self) {
        self.rotating = false;
    }

    /// Get list of available key versions (for admin/debugging)
    pub fn available_versions(&self) -> Vec<u32> {
        let mut versions: Vec<u32> = self.key_cache.keys().copied().collect();
        versions.sort_unstable();
        versions
    }

    /// Clear key cache (for security, before shutdown)
    ///
    /// Keys will be re-derived on next access
    pub fn clear_cache(&mut self) {
        self.key_cache.clear();
    }
}

impl Drop for KeyManager {
    fn drop(&mut self) {
        // Zeroize master key
        self.master_key.zeroize();
        // Clear cache (keys are ZeroizeOnDrop)
        self.key_cache.clear();
    }
}

impl std::fmt::Debug for KeyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyManager")
            .field("master_key", &"[REDACTED]")
            .field("current_version", &self.current_version)
            .field("cached_versions", &self.key_cache.len())
            .field("rotating", &self.rotating)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_key_pair_creation() {
        let bytes = [42u8; XTS_KEY_SIZE];
        let pair = XtsKeyPair::from_bytes(bytes);

        assert_eq!(pair.key1()[0], 42);
        assert_eq!(pair.key2()[0], 42);
        assert_eq!(pair.to_bytes(), bytes);
    }

    #[test]
    #[serial]
    fn test_key_pair_debug() {
        let bytes = [42u8; XTS_KEY_SIZE];
        let pair = XtsKeyPair::from_bytes(bytes);
        let debug_str = format!("{:?}", pair);

        // Should not leak key material
        assert!(debug_str.contains("REDACTED"));
        assert!(!debug_str.contains("42"));
    }

    #[test]
    #[serial]
    fn test_key_manager_creation() {
        let master_key = [0u8; MASTER_KEY_SIZE];
        let manager = KeyManager::new(master_key);

        assert_eq!(manager.current_version(), 1);
        assert!(!manager.is_rotating());
    }

    #[test]
    #[serial]
    fn test_key_derivation() {
        let master_key = [42u8; MASTER_KEY_SIZE];
        let mut manager = KeyManager::new(master_key);

        // Derive key for version 1
        let key1 = manager.get_key(1).unwrap();
        assert_eq!(key1.key1().len(), 32);
        assert_eq!(key1.key2().len(), 32);

        // Should be cached
        assert_eq!(manager.available_versions(), vec![1]);
    }

    #[test]
    #[serial]
    fn test_key_derivation_deterministic() {
        let master_key = [7u8; MASTER_KEY_SIZE];

        let mut manager1 = KeyManager::new(master_key);
        let mut manager2 = KeyManager::new(master_key);

        let key1 = manager1.get_key(1).unwrap().to_bytes();
        let key2 = manager2.get_key(1).unwrap().to_bytes();

        // Same master key + version = same derived key
        assert_eq!(key1, key2);
    }

    #[test]
    #[serial]
    fn test_different_versions_different_keys() {
        let master_key = [13u8; MASTER_KEY_SIZE];
        let mut manager = KeyManager::new(master_key);

        let key_v1 = manager.get_key(1).unwrap().to_bytes();
        let key_v2 = manager.get_key(2).unwrap().to_bytes();

        // Different versions = different keys
        assert_ne!(key_v1, key_v2);
    }

    #[test]
    #[serial]
    fn test_key_rotation() {
        let master_key = [99u8; MASTER_KEY_SIZE];
        let mut manager = KeyManager::new(master_key);

        assert_eq!(manager.current_version(), 1);
        assert!(!manager.is_rotating());

        // Begin rotation
        let new_version = manager.rotate().unwrap();
        assert_eq!(new_version, 2);
        assert_eq!(manager.current_version(), 2);
        assert!(manager.is_rotating());

        // Can't rotate while in progress
        let err = manager.rotate();
        assert!(matches!(err, Err(EncryptionError::KeyRotationInProgress)));

        // Complete rotation
        manager.complete_rotation();
        assert!(!manager.is_rotating());

        // Old key still accessible
        assert!(manager.get_key(1).is_ok());
        assert!(manager.get_key(2).is_ok());
    }

    #[test]
    #[serial]
    fn test_available_versions() {
        let master_key = [5u8; MASTER_KEY_SIZE];
        let mut manager = KeyManager::new(master_key);

        // Access some keys
        manager.get_key(1).unwrap();
        manager.get_key(3).unwrap();
        manager.get_key(2).unwrap();

        let versions = manager.available_versions();
        assert_eq!(versions, vec![1, 2, 3]); // Sorted
    }

    #[test]
    #[serial]
    fn test_clear_cache() {
        let master_key = [11u8; MASTER_KEY_SIZE];
        let mut manager = KeyManager::new(master_key);

        manager.get_key(1).unwrap();
        manager.get_key(2).unwrap();
        assert_eq!(manager.available_versions().len(), 2);

        manager.clear_cache();
        assert_eq!(manager.available_versions().len(), 0);

        // Keys can be re-derived
        manager.get_key(1).unwrap();
        assert_eq!(manager.available_versions().len(), 1);
    }

    #[test]
    #[serial]
    fn test_manager_debug() {
        let master_key = [77u8; MASTER_KEY_SIZE];
        let manager = KeyManager::new(master_key);
        let debug_str = format!("{:?}", manager);

        // Should not leak master key
        assert!(debug_str.contains("REDACTED"));
        assert!(!debug_str.contains("77"));

        // Should show metadata
        assert!(debug_str.contains("current_version"));
    }

    #[test]
    #[serial]
    fn test_from_env_missing() {
        // Save current value if exists
        let original = std::env::var("SPACE_MASTER_KEY").ok();

        // Clear env var
        std::env::remove_var("SPACE_MASTER_KEY");

        let result = KeyManager::from_env();

        // Restore original value
        if let Some(val) = original {
            std::env::set_var("SPACE_MASTER_KEY", val);
        }

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EncryptionError::InvalidConfiguration(_)
        ));
    }

    #[test]
    #[serial]
    fn test_from_env_valid() {
        // Save current value if exists
        let original = std::env::var("SPACE_MASTER_KEY").ok();

        // Set valid hex key
        let master_key = [88u8; MASTER_KEY_SIZE];
        let hex_key = hex::encode(master_key);
        std::env::set_var("SPACE_MASTER_KEY", &hex_key);

        let result = KeyManager::from_env();

        // Restore original value
        if let Some(val) = original {
            std::env::set_var("SPACE_MASTER_KEY", val);
        } else {
            std::env::remove_var("SPACE_MASTER_KEY");
        }

        let manager = result.expect("Should create KeyManager from valid hex");
        assert_eq!(manager.current_version(), 1);
    }

    #[test]
    #[serial]
    fn test_from_env_invalid_hex() {
        // Save current value if exists
        let original = std::env::var("SPACE_MASTER_KEY").ok();

        // Set invalid hex - must happen right before the test
        std::env::set_var("SPACE_MASTER_KEY", "not-valid-hex!");

        // Immediately call from_env() before any other test can change it
        let result = KeyManager::from_env();

        // Restore original value immediately
        if let Some(val) = original {
            std::env::set_var("SPACE_MASTER_KEY", val);
        } else {
            std::env::remove_var("SPACE_MASTER_KEY");
        }

        // Assert after cleanup
        assert!(
            result.is_err(),
            "Expected error for invalid hex, got: {:?}",
            result
        );
        if let Err(e) = result {
            // Verify it's the right kind of error
            assert!(matches!(e, EncryptionError::InvalidConfiguration(_)));
        }
    }

    #[test]
    #[serial]
    fn test_from_env_wrong_length() {
        // Save current value if exists
        let original = std::env::var("SPACE_MASTER_KEY").ok();

        // Too short
        let hex_key = hex::encode([1u8; 16]);
        std::env::set_var("SPACE_MASTER_KEY", &hex_key);

        let result = KeyManager::from_env();

        // Restore original value
        if let Some(val) = original {
            std::env::set_var("SPACE_MASTER_KEY", val);
        } else {
            std::env::remove_var("SPACE_MASTER_KEY");
        }

        assert!(matches!(
            result.unwrap_err(),
            EncryptionError::InvalidKeyLength { .. }
        ));
    }
}

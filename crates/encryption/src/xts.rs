//! XTS-AES-256 Encryption/Decryption
//!
//! Implements XTS mode (XEX-based tweaked-codebook mode with ciphertext stealing)
//! for disk encryption. XTS is specifically designed for encrypting data at rest
//! with sector-level granularity.
//!
//! ## Features
//!
//! - XTS-AES-256 with 512-bit keys (two AES-256 keys)
//! - Deterministic tweaks derived from content hashes
//! - Hardware acceleration via AES-NI when available
//! - Preserves deduplication (same plaintext + tweak = same ciphertext)
//!
//! ## Security Properties
//!
//! - Confidentiality: AES-256 strength
//! - Deterministic: Enables deduplication
//! - No authentication: Use Poly1305 MAC separately (see mac.rs)

use crate::error::{EncryptionError, Result};
use crate::keymanager::XtsKeyPair;
use crate::policy::EncryptionMetadata;
use aes::Aes256;
use cipher::KeyInit;
use xts_mode::Xts128;

/// XTS block size (128 bits / 16 bytes)
const XTS_BLOCK_SIZE: usize = 16;

/// Minimum data size for XTS (one block)
const MIN_SECTOR_SIZE: usize = XTS_BLOCK_SIZE;

// Hardware feature detection (e.g. AES-NI) is handled internally by the `aes`
// crate via the `cpufeatures` integration, so we can rely on it directly.

/// Encrypt data using XTS-AES-256
///
/// # Arguments
///
/// * `plaintext` - Data to encrypt (must be >= 16 bytes)
/// * `key_pair` - XTS key pair (512 bits)
/// * `tweak` - 128-bit tweak (derived from content hash)
///
/// # Returns
///
/// Encrypted data (same length as plaintext)
///
/// # Errors
///
/// Returns error if:
/// - Plaintext is too short (< 16 bytes)
/// - Key pair is invalid
/// - Encryption operation fails
pub fn encrypt(plaintext: &[u8], key_pair: &XtsKeyPair, tweak: &[u8; 16]) -> Result<Vec<u8>> {
    // Validate input size
    if plaintext.len() < MIN_SECTOR_SIZE {
        return Err(EncryptionError::InvalidCiphertextLength(plaintext.len()));
    }

    // Create cipher instances using KeyInit trait
    let cipher1 = Aes256::new(key_pair.key1().into());
    let cipher2 = Aes256::new(key_pair.key2().into());

    // Create XTS instance
    let xts = Xts128::<Aes256>::new(cipher1, cipher2);

    // Prepare output buffer
    let mut ciphertext = plaintext.to_vec();

    // Encrypt in place - xts-mode expects [u8; 16] directly
    xts.encrypt_sector(&mut ciphertext, *tweak);

    Ok(ciphertext)
}

/// Decrypt data using XTS-AES-256
///
/// # Arguments
///
/// * `ciphertext` - Encrypted data (must be >= 16 bytes)
/// * `key_pair` - XTS key pair (512 bits)
/// * `tweak` - 128-bit tweak (same as used for encryption)
///
/// # Returns
///
/// Decrypted data (same length as ciphertext)
///
/// # Errors
///
/// Returns error if:
/// - Ciphertext is too short (< 16 bytes)
/// - Key pair is invalid
/// - Decryption operation fails
pub fn decrypt(ciphertext: &[u8], key_pair: &XtsKeyPair, tweak: &[u8; 16]) -> Result<Vec<u8>> {
    // Validate input size
    if ciphertext.len() < MIN_SECTOR_SIZE {
        return Err(EncryptionError::InvalidCiphertextLength(ciphertext.len()));
    }

    // Create cipher instances using KeyInit trait
    let cipher1 = Aes256::new(key_pair.key1().into());
    let cipher2 = Aes256::new(key_pair.key2().into());

    // Create XTS instance
    let xts = Xts128::<Aes256>::new(cipher1, cipher2);

    // Prepare output buffer
    let mut plaintext = ciphertext.to_vec();

    // Decrypt in place - xts-mode expects [u8; 16] directly
    xts.decrypt_sector(&mut plaintext, *tweak);

    Ok(plaintext)
}

/// Encrypt segment with metadata creation
///
/// Convenience function that encrypts data and creates metadata in one call.
/// This is the primary interface for the pipeline.
///
/// # Arguments
///
/// * `plaintext` - Segment data to encrypt
/// * `key_pair` - XTS key pair
/// * `key_version` - Key version (for metadata)
/// * `tweak` - Deterministic tweak derived from content hash
///
/// # Returns
///
/// Tuple of (ciphertext, metadata with encryption info)
pub fn encrypt_segment(
    plaintext: &[u8],
    key_pair: &XtsKeyPair,
    key_version: u32,
    tweak: [u8; 16],
) -> Result<(Vec<u8>, EncryptionMetadata)> {
    // Encrypt the data
    let ciphertext = encrypt(plaintext, key_pair, &tweak)?;

    // Create metadata
    let metadata = EncryptionMetadata::new_xts(key_version, tweak, ciphertext.len() as u32);

    Ok((ciphertext, metadata))
}

/// Decrypt segment using metadata
///
/// Convenience function that extracts info from metadata and decrypts.
/// This is the primary interface for the pipeline.
///
/// # Arguments
///
/// * `ciphertext` - Encrypted segment data
/// * `key_pair` - XTS key pair (must match key_version in metadata)
/// * `metadata` - Encryption metadata containing tweak and length
///
/// # Returns
///
/// Decrypted plaintext data
pub fn decrypt_segment(
    ciphertext: &[u8],
    key_pair: &XtsKeyPair,
    metadata: &EncryptionMetadata,
) -> Result<Vec<u8>> {
    // Verify metadata is present
    if !metadata.is_encrypted() {
        return Err(EncryptionError::MissingMetadata);
    }

    // Extract tweak from metadata
    let tweak = metadata
        .require_tweak()
        .map_err(|e| EncryptionError::CorruptedMetadata(e.to_string()))?;

    // Verify length matches
    let expected_len = metadata
        .ciphertext_len
        .ok_or_else(|| EncryptionError::MissingMetadata)?;

    if ciphertext.len() != expected_len as usize {
        return Err(EncryptionError::InvalidCiphertextLength(ciphertext.len()));
    }

    // Decrypt the data
    decrypt(ciphertext, key_pair, &tweak)
}

/// Derive deterministic tweak from content hash
///
/// Takes a BLAKE3 hash (32 bytes) and extracts the first 16 bytes as a tweak.
/// This ensures identical content produces identical tweaks, preserving dedup.
///
/// # Arguments
///
/// * `content_hash` - BLAKE3 hash of compressed data (32 bytes)
///
/// # Returns
///
/// 16-byte tweak for XTS mode
pub fn derive_tweak_from_hash(content_hash: &[u8]) -> [u8; 16] {
    let mut tweak = [0u8; 16];

    // Take first 16 bytes of hash
    let copy_len = content_hash.len().min(16);
    tweak[..copy_len].copy_from_slice(&content_hash[..copy_len]);

    tweak
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymanager::{KeyManager, MASTER_KEY_SIZE};

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        // Setup
        let master_key = [42u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let plaintext = b"Hello SPACE! This is a test of XTS-AES-256 encryption.";
        let tweak = [1u8; 16];

        // Encrypt
        let ciphertext = encrypt(plaintext, key_pair, &tweak).unwrap();

        // Verify ciphertext is different from plaintext
        assert_ne!(ciphertext, plaintext);
        assert_eq!(ciphertext.len(), plaintext.len());

        // Decrypt
        let decrypted = decrypt(&ciphertext, key_pair, &tweak).unwrap();

        // Verify round-trip
        assert_eq!(decrypted, plaintext);

        println!("✅ Encrypt/decrypt roundtrip successful");
    }

    #[test]
    fn test_deterministic_encryption() {
        // Setup
        let master_key = [7u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let plaintext = b"Deterministic encryption test data for deduplication.";
        let tweak = [5u8; 16];

        // Encrypt twice with same key and tweak
        let ciphertext1 = encrypt(plaintext, key_pair, &tweak).unwrap();
        let ciphertext2 = encrypt(plaintext, key_pair, &tweak).unwrap();

        // Should produce identical ciphertext (deterministic)
        assert_eq!(ciphertext1, ciphertext2);

        println!("✅ Deterministic encryption verified");
    }

    #[test]
    fn test_different_tweaks_different_ciphertext() {
        // Setup
        let master_key = [13u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let plaintext = b"Same plaintext, different tweaks should produce different ciphertext.";
        let tweak1 = [1u8; 16];
        let tweak2 = [2u8; 16];

        // Encrypt with different tweaks
        let ciphertext1 = encrypt(plaintext, key_pair, &tweak1).unwrap();
        let ciphertext2 = encrypt(plaintext, key_pair, &tweak2).unwrap();

        // Should produce different ciphertext
        assert_ne!(ciphertext1, ciphertext2);

        println!("✅ Different tweaks produce different ciphertext");
    }

    #[test]
    fn test_minimum_size() {
        // Setup
        let master_key = [99u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();
        let tweak = [0u8; 16];

        // Test minimum valid size (16 bytes)
        let min_plaintext = [42u8; 16];
        let result = encrypt(&min_plaintext, key_pair, &tweak);
        assert!(result.is_ok());

        // Test below minimum (15 bytes)
        let too_small = [42u8; 15];
        let result = encrypt(&too_small, key_pair, &tweak);
        assert!(result.is_err());

        println!("✅ Minimum size validation works");
    }

    #[test]
    fn test_large_data() {
        // Setup
        let master_key = [88u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();
        let tweak = [9u8; 16];

        // Test with 4MB (typical segment size)
        let large_data = vec![42u8; 4 * 1024 * 1024];

        let ciphertext = encrypt(&large_data, key_pair, &tweak).unwrap();
        assert_eq!(ciphertext.len(), large_data.len());

        let decrypted = decrypt(&ciphertext, key_pair, &tweak).unwrap();
        assert_eq!(decrypted, large_data);

        println!("✅ Large data (4MB) encryption successful");
    }

    #[test]
    fn test_encrypt_segment_with_metadata() {
        // Setup
        let master_key = [55u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let plaintext = b"Test segment encryption with metadata creation.";
        let tweak = [7u8; 16];

        // Encrypt with metadata
        let (ciphertext, metadata) = encrypt_segment(plaintext, key_pair, 1, tweak).unwrap();

        // Verify metadata
        assert!(metadata.is_encrypted());
        assert_eq!(metadata.encryption_version, Some(1));
        assert_eq!(metadata.key_version, Some(1));
        assert_eq!(metadata.tweak_nonce, Some(tweak));
        assert_eq!(metadata.ciphertext_len, Some(ciphertext.len() as u32));

        println!("✅ Segment encryption with metadata works");
    }

    #[test]
    fn test_decrypt_segment_with_metadata() {
        // Setup
        let master_key = [66u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let plaintext = b"Test segment decryption using metadata.";
        let tweak = [8u8; 16];

        // Encrypt with metadata
        let (ciphertext, metadata) = encrypt_segment(plaintext, key_pair, 1, tweak).unwrap();

        // Decrypt using metadata
        let decrypted = decrypt_segment(&ciphertext, key_pair, &metadata).unwrap();

        // Verify round-trip
        assert_eq!(decrypted, plaintext);

        println!("✅ Segment decryption with metadata works");
    }

    #[test]
    fn test_decrypt_unencrypted_metadata_fails() {
        // Setup
        let master_key = [77u8; MASTER_KEY_SIZE];
        let mut km = KeyManager::new(master_key);
        let key_pair = km.get_key(1).unwrap();

        let ciphertext = b"Some encrypted data";
        let unencrypted_metadata = EncryptionMetadata::default();

        // Should fail with unencrypted metadata
        let result = decrypt_segment(ciphertext, key_pair, &unencrypted_metadata);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EncryptionError::MissingMetadata
        ));

        println!("✅ Unencrypted metadata detection works");
    }

    #[test]
    fn test_derive_tweak_from_hash() {
        // Test with 32-byte hash (BLAKE3 output)
        let hash = [42u8; 32];
        let tweak = derive_tweak_from_hash(&hash);

        assert_eq!(tweak.len(), 16);
        assert_eq!(tweak[0], 42);
        assert_eq!(tweak[15], 42);

        // Test determinism
        let tweak2 = derive_tweak_from_hash(&hash);
        assert_eq!(tweak, tweak2);

        println!("✅ Tweak derivation from hash works");
    }

    #[test]
    fn test_wrong_key_produces_garbage() {
        // Setup with two different keys
        let master_key1 = [11u8; MASTER_KEY_SIZE];
        let master_key2 = [22u8; MASTER_KEY_SIZE];

        let mut km1 = KeyManager::new(master_key1);
        let mut km2 = KeyManager::new(master_key2);

        let key_pair1 = km1.get_key(1).unwrap();
        let key_pair2 = km2.get_key(1).unwrap();

        let plaintext = b"Secret message that should not decrypt with wrong key.";
        let tweak = [3u8; 16];

        // Encrypt with key1
        let ciphertext = encrypt(plaintext, key_pair1, &tweak).unwrap();

        // Decrypt with key2 (wrong key)
        let wrong_decrypt = decrypt(&ciphertext, key_pair2, &tweak).unwrap();

        // Should not match original plaintext
        assert_ne!(wrong_decrypt, plaintext);

        println!("✅ Wrong key produces garbage (as expected)");
    }
}

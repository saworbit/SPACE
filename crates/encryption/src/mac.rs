//! BLAKE3-based Message Authentication Code
//! 
//! Provides integrity verification for encrypted segments using BLAKE3 in keyed mode.
//! The MAC is computed over the ciphertext plus metadata to detect tampering
//! or corruption.
//! 
//! ## Security Properties
//! 
//! - Integrity: Detects any modification to ciphertext or metadata
//! - Authentication: Verifies data hasn't been tampered with
//! - Fast: BLAKE3 is extremely fast (faster than Poly1305)
//! - Simple: No block size limitations
//! 
//! ## Usage Pattern
//! 
//! 1. Encrypt data with XTS
//! 2. Compute MAC over ciphertext + metadata
//! 3. Store MAC in metadata.integrity_tag
//! 
//! On read:
//! 1. Fetch ciphertext + metadata
//! 2. Verify MAC matches
//! 3. Decrypt if MAC is valid

use crate::error::{EncryptionError, Result};
use crate::policy::EncryptionMetadata;
use blake3;

/// MAC tag size (128 bits / 16 bytes)
pub const MAC_TAG_SIZE: usize = 16;

/// Derive MAC key from XTS keys using BLAKE3
/// 
/// We can't reuse XTS keys directly for MAC, so we derive a separate
/// MAC key using BLAKE3 as a KDF.
/// 
/// # Arguments
/// 
/// * `xts_key1` - First XTS key (32 bytes)
/// * `xts_key2` - Second XTS key (32 bytes)
/// 
/// # Returns
/// 
/// 32-byte MAC key
fn derive_mac_key(xts_key1: &[u8; 32], xts_key2: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    
    // Context string to domain-separate from other uses
    hasher.update(b"SPACE-BLAKE3-MAC-KEY-V1");
    hasher.update(xts_key1);
    hasher.update(xts_key2);
    
    let hash = hasher.finalize();
    *hash.as_bytes()
}

/// Compute BLAKE3-based MAC over ciphertext and metadata
/// 
/// Uses BLAKE3 in keyed mode as a MAC. This provides:
/// - Faster performance than Poly1305
/// - Simpler API (no block size constraints)
/// - Equivalent cryptographic security
/// 
/// The MAC is computed over:
/// - Ciphertext (variable length)
/// - Metadata (serialized to bytes)
/// 
/// This ensures integrity of both the encrypted data and its metadata.
/// 
/// # Arguments
/// 
/// * `ciphertext` - Encrypted data
/// * `metadata` - Encryption metadata (without integrity_tag set)
/// * `xts_key1` - First XTS key (for MAC key derivation)
/// * `xts_key2` - Second XTS key (for MAC key derivation)
/// 
/// # Returns
/// 
/// 16-byte MAC tag
pub fn compute_mac(
    ciphertext: &[u8],
    metadata: &EncryptionMetadata,
    xts_key1: &[u8; 32],
    xts_key2: &[u8; 32],
) -> Result<[u8; 16]> {
    // Derive MAC key from XTS keys
    let mac_key = derive_mac_key(xts_key1, xts_key2);
    
    // Use BLAKE3 in keyed mode
    let mut hasher = blake3::Hasher::new_keyed(&mac_key);
    
    // Hash ciphertext
    hasher.update(ciphertext);
    
    // Serialize and hash metadata
    let metadata_bytes = serialize_metadata_for_mac(metadata)?;
    hasher.update(&metadata_bytes);
    
    // Finalize and take first 16 bytes as MAC tag
    let hash = hasher.finalize();
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&hash.as_bytes()[0..16]);
    
    Ok(tag)
}

/// Verify BLAKE3-based MAC
/// 
/// Recomputes the MAC and compares it with the stored tag in constant time.
/// 
/// # Arguments
/// 
/// * `ciphertext` - Encrypted data
/// * `metadata` - Encryption metadata (with integrity_tag set)
/// * `xts_key1` - First XTS key (for MAC key derivation)
/// * `xts_key2` - Second XTS key (for MAC key derivation)
/// 
/// # Returns
/// 
/// Ok(()) if MAC is valid, Error if verification fails
pub fn verify_mac(
    ciphertext: &[u8],
    metadata: &EncryptionMetadata,
    xts_key1: &[u8; 32],
    xts_key2: &[u8; 32],
) -> Result<()> {
    // Extract stored tag
    let stored_tag = metadata.require_integrity_tag()
        .map_err(|_| EncryptionError::MissingIntegrityTag)?;
    
    // Compute expected tag (using metadata without integrity_tag)
    let mut metadata_for_mac = metadata.clone();
    metadata_for_mac.integrity_tag = None;
    
    let computed_tag = compute_mac(ciphertext, &metadata_for_mac, xts_key1, xts_key2)?;
    
    // Constant-time comparison
    if constant_time_eq(&stored_tag, &computed_tag) {
        Ok(())
    } else {
        Err(EncryptionError::IntegrityFailure)
    }
}

/// Serialize metadata for MAC computation
/// 
/// Creates a deterministic byte representation of metadata.
/// Uses simple concatenation of fields.
fn serialize_metadata_for_mac(metadata: &EncryptionMetadata) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    
    // Encryption version (2 bytes)
    if let Some(version) = metadata.encryption_version {
        bytes.extend_from_slice(&version.to_le_bytes());
    }
    
    // Key version (4 bytes)
    if let Some(key_version) = metadata.key_version {
        bytes.extend_from_slice(&key_version.to_le_bytes());
    }
    
    // Tweak nonce (16 bytes)
    if let Some(tweak) = metadata.tweak_nonce {
        bytes.extend_from_slice(&tweak);
    }
    
    // Ciphertext length (4 bytes)
    if let Some(len) = metadata.ciphertext_len {
        bytes.extend_from_slice(&len.to_le_bytes());
    }
    
    Ok(bytes)
}

/// Constant-time equality comparison
/// 
/// Prevents timing attacks by always comparing all bytes.
fn constant_time_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut result = 0u8;
    for i in 0..16 {
        result |= a[i] ^ b[i];
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_mac_key() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        
        let mac_key = derive_mac_key(&key1, &key2);
        
        assert_eq!(mac_key.len(), 32);
        
        // Verify determinism
        let mac_key2 = derive_mac_key(&key1, &key2);
        assert_eq!(mac_key, mac_key2);
        
        // Verify different keys produce different MAC keys
        let key3 = [3u8; 32];
        let mac_key3 = derive_mac_key(&key1, &key3);
        assert_ne!(mac_key, mac_key3);
        
        println!("✅ MAC key derivation works");
    }

    #[test]
    fn test_compute_mac() {
        let ciphertext = b"encrypted data here";
        let metadata = EncryptionMetadata::new_xts(1, [5u8; 16], ciphertext.len() as u32);
        let key1 = [42u8; 32];
        let key2 = [99u8; 32];
        
        let tag = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        
        assert_eq!(tag.len(), MAC_TAG_SIZE);
        
        // Verify determinism
        let tag2 = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        assert_eq!(tag, tag2);
        
        println!("✅ MAC computation works");
    }

    #[test]
    fn test_different_data_different_mac() {
        let ciphertext1 = b"encrypted data one";
        let ciphertext2 = b"encrypted data two";
        let metadata = EncryptionMetadata::new_xts(1, [5u8; 16], 18);
        let key1 = [42u8; 32];
        let key2 = [99u8; 32];
        
        let tag1 = compute_mac(ciphertext1, &metadata, &key1, &key2).unwrap();
        let tag2 = compute_mac(ciphertext2, &metadata, &key1, &key2).unwrap();
        
        // Different data should produce different MACs
        assert_ne!(tag1, tag2);
        
        println!("✅ Different data produces different MACs");
    }

    #[test]
    fn test_different_metadata_different_mac() {
        let ciphertext = b"encrypted data here";
        let metadata1 = EncryptionMetadata::new_xts(1, [5u8; 16], ciphertext.len() as u32);
        let metadata2 = EncryptionMetadata::new_xts(2, [5u8; 16], ciphertext.len() as u32);
        let key1 = [42u8; 32];
        let key2 = [99u8; 32];
        
        let tag1 = compute_mac(ciphertext, &metadata1, &key1, &key2).unwrap();
        let tag2 = compute_mac(ciphertext, &metadata2, &key1, &key2).unwrap();
        
        // Different metadata should produce different MACs
        assert_ne!(tag1, tag2);
        
        println!("✅ Different metadata produces different MACs");
    }

    #[test]
    fn test_verify_mac_valid() {
        let ciphertext = b"test encrypted segment data";
        let mut metadata = EncryptionMetadata::new_xts(1, [7u8; 16], ciphertext.len() as u32);
        let key1 = [11u8; 32];
        let key2 = [22u8; 32];
        
        // Compute and store MAC
        let tag = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        metadata.set_integrity_tag(tag);
        
        // Verify should succeed
        let result = verify_mac(ciphertext, &metadata, &key1, &key2);
        assert!(result.is_ok());
        
        println!("✅ Valid MAC verification works");
    }

    #[test]
    fn test_verify_mac_invalid() {
        let ciphertext = b"test encrypted segment data";
        let mut metadata = EncryptionMetadata::new_xts(1, [7u8; 16], ciphertext.len() as u32);
        let key1 = [11u8; 32];
        let key2 = [22u8; 32];
        
        // Compute and store MAC
        let tag = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        metadata.set_integrity_tag(tag);
        
        // Tamper with ciphertext
        let mut tampered = ciphertext.to_vec();
        tampered[0] ^= 1;
        
        // Verify should fail
        let result = verify_mac(&tampered, &metadata, &key1, &key2);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EncryptionError::IntegrityFailure));
        
        println!("✅ Tampered data detected");
    }

    #[test]
    fn test_verify_mac_wrong_key() {
        let ciphertext = b"test encrypted segment data";
        let mut metadata = EncryptionMetadata::new_xts(1, [7u8; 16], ciphertext.len() as u32);
        let key1 = [11u8; 32];
        let key2 = [22u8; 32];
        let wrong_key1 = [33u8; 32];
        let wrong_key2 = [44u8; 32];
        
        // Compute and store MAC with correct keys
        let tag = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        metadata.set_integrity_tag(tag);
        
        // Verify with wrong keys should fail
        let result = verify_mac(ciphertext, &metadata, &wrong_key1, &wrong_key2);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EncryptionError::IntegrityFailure));
        
        println!("✅ Wrong key detection works");
    }

    #[test]
    fn test_verify_mac_missing_tag() {
        let ciphertext = b"test data";
        let metadata = EncryptionMetadata::new_xts(1, [7u8; 16], ciphertext.len() as u32);
        let key1 = [11u8; 32];
        let key2 = [22u8; 32];
        
        // Metadata without integrity tag
        let result = verify_mac(ciphertext, &metadata, &key1, &key2);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EncryptionError::MissingIntegrityTag));
        
        println!("✅ Missing tag detection works");
    }

    #[test]
    fn test_serialize_metadata_for_mac() {
        let metadata = EncryptionMetadata::new_xts(1, [5u8; 16], 1024);
        
        let bytes = serialize_metadata_for_mac(&metadata).unwrap();
        
        // Should contain version (2) + key_version (4) + tweak (16) + len (4) = 26 bytes
        assert_eq!(bytes.len(), 26);
        
        // Verify determinism
        let bytes2 = serialize_metadata_for_mac(&metadata).unwrap();
        assert_eq!(bytes, bytes2);
        
        println!("✅ Metadata serialization works");
    }

    #[test]
    fn test_constant_time_eq() {
        let a = [1u8; 16];
        let b = [1u8; 16];
        let c = [2u8; 16];
        
        assert!(constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &c));
        
        // Verify single bit difference is detected
        let mut d = [1u8; 16];
        d[15] ^= 1;
        assert!(!constant_time_eq(&a, &d));
        
        println!("✅ Constant-time comparison works");
    }

    #[test]
    fn test_mac_with_large_data() {
        // Test with 4MB segment
        let ciphertext = vec![42u8; 4 * 1024 * 1024];
        let metadata = EncryptionMetadata::new_xts(1, [9u8; 16], ciphertext.len() as u32);
        let key1 = [77u8; 32];
        let key2 = [88u8; 32];
        
        // Compute MAC
        let tag = compute_mac(&ciphertext, &metadata, &key1, &key2).unwrap();
        assert_eq!(tag.len(), MAC_TAG_SIZE);
        
        // Verify
        let mut metadata_with_tag = metadata.clone();
        metadata_with_tag.set_integrity_tag(tag);
        
        let result = verify_mac(&ciphertext, &metadata_with_tag, &key1, &key2);
        assert!(result.is_ok());
        
        println!("✅ Large data (4MB) MAC works");
    }

    #[test]
    fn test_metadata_tampering_detected() {
        let ciphertext = b"test data";
        let mut metadata = EncryptionMetadata::new_xts(1, [7u8; 16], ciphertext.len() as u32);
        let key1 = [11u8; 32];
        let key2 = [22u8; 32];
        
        // Compute and store MAC
        let tag = compute_mac(ciphertext, &metadata, &key1, &key2).unwrap();
        metadata.set_integrity_tag(tag);
        
        // Tamper with metadata (change key version)
        let mut tampered_metadata = metadata.clone();
        tampered_metadata.key_version = Some(99);
        
        // Verify should fail
        let result = verify_mac(ciphertext, &tampered_metadata, &key1, &key2);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EncryptionError::IntegrityFailure));
        
        println!("✅ Metadata tampering detected");
    }
}
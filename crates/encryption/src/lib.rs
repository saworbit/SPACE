//! # SPACE Encryption Module
//!
//! Provides per-segment encryption with deduplication preservation for the SPACE
//! storage platform. This crate implements Phase 3 encryption features.
//!
//! ## Features
//!
//! - **XTS-AES-256**: Disk encryption mode with deterministic tweaks
//! - **Poly1305 MAC**: Integrity verification for encrypted segments
//! - **Key Management**: Versioned keys with rotation support
//! - **Dedup Preservation**: Identical plaintext → identical ciphertext
//! - **Hardware Acceleration**: AES-NI support when available
//!
//! ## Phase Roadmap
//!
//! - **Phase 3.1 (Current)**: XTS-AES-256 with Poly1305 MAC (MVP)
//! - **Phase 3.2 (Future)**: ChaCha20-Poly1305 with convergent encryption
//! - **Phase 3.3 (Future)**: MLE with post-quantum key wrapping
//!
//! ## Usage Example
//!
//! ```rust,ignore
//! use encryption::{EncryptionPolicy, EncryptionMetadata};
//!
//! // Configure encryption policy
//! let policy = EncryptionPolicy::XtsAes256 { key_version: 1 };
//!
//! // Encryption metadata is created during encryption
//! let metadata = EncryptionMetadata::new_xts(1, tweak, ciphertext.len() as u32);
//!
//! // Check if segment is encrypted
//! if metadata.is_encrypted() {
//!     println!("Segment encrypted with key version {}",
//!              metadata.require_key_version().unwrap());
//! }
//! ```
//!
//! ## Security Considerations
//!
//! - **Key Storage**: Keys must be stored securely (TPM, KMS, or env vars)
//! - **Key Rotation**: Old keys must remain available for reading old segments
//! - **Tweak Derivation**: Tweaks are derived from content hashes (deterministic)
//! - **Integrity**: Always verify MAC before trusting decrypted data
//!
//! ## Architecture Integration
//!
//! This crate integrates with SPACE components:
//!
//! ```text
//! capsule-registry/pipeline.rs
//!     ↓ (uses)
//! encryption crate
//!     ↓ (provides)
//! - Policy types
//! - Encryption/decryption functions
//! - Key management
//! - Integrity verification
//! ```

// Module declarations
pub mod error;
pub mod keymanager;
pub mod mac;
pub mod policy;
pub mod xts;

// Re-exports for convenience
pub use error::{EncryptionError, Result};
pub use keymanager::{KeyManager, XtsKeyPair};
pub use mac::{compute_mac, verify_mac, MAC_TAG_SIZE};
pub use policy::{EncryptionMetadata, EncryptionPolicy, EncryptionStats};
pub use xts::{decrypt_segment, derive_tweak_from_hash, encrypt_segment};

// Version information
/// Encryption crate version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current encryption format version (XTS-AES-256 with Poly1305)
pub const ENCRYPTION_FORMAT_VERSION: u16 = 1;

/// Tweak/nonce length for XTS mode (128 bits)
pub const TWEAK_LENGTH: usize = 16;

/// Poly1305 MAC tag length (128 bits)
pub const MAC_TAG_LENGTH: usize = 16;

/// XTS key length (512 bits = 2 x AES-256 keys)
pub const XTS_KEY_LENGTH: usize = 64;

// Feature-gated modules (future phases)
#[cfg(feature = "experimental")]
pub mod experimental {
    //! Experimental encryption features (Phase 3.2+)
    //!
    //! These features are under development and may change.
    //! Not recommended for production use.
}

#[cfg(feature = "pqc")]
pub mod pqc {
    //! Post-quantum cryptography support (Phase 3.3)
    //!
    //! Provides ML-KEM (Kyber) key encapsulation for quantum resistance.
}

#[cfg(feature = "tee")]
pub mod tee {
    //! Trusted Execution Environment support (Phase 3.3)
    //!
    //! Integration with SGX/SEV for confidential computing.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(ENCRYPTION_FORMAT_VERSION, 1);
        assert_eq!(TWEAK_LENGTH, 16);
        assert_eq!(MAC_TAG_LENGTH, 16);
        assert_eq!(XTS_KEY_LENGTH, 64);
    }

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
        println!("Encryption crate version: {}", VERSION);
    }

    #[test]
    fn test_exports() {
        // Verify we can construct types from re-exports
        let _policy = EncryptionPolicy::default();
        let _metadata = EncryptionMetadata::default();
        let _stats = EncryptionStats::new();

        // Verify error types
        let err = EncryptionError::EncryptionNotEnabled;
        assert!(err.to_string().contains("not enabled"));
    }

    #[test]
    fn test_policy_default() {
        let policy = EncryptionPolicy::default();
        assert_eq!(policy, EncryptionPolicy::None);
        assert!(!policy.is_enabled());
    }

    #[test]
    fn test_metadata_default() {
        let metadata = EncryptionMetadata::default();
        assert!(!metadata.is_encrypted());
        assert!(!metadata.has_integrity_tag());
    }
}

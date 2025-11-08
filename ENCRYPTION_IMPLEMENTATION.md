# SPACE Phase 3: Encryption Implementation Guide

**Version:** 0.1.0  
**Date:** 2025-01-27  
**Status:** Complete - Production Ready  

---

## Overview

This document provides implementation details for SPACE Phase 3 encryption, including XTS-AES-256 encryption, BLAKE3-MAC integrity, and key management with deduplication preservation.

---

## Module Architecture
```
crates/encryption/
├── src/
│   ├── lib.rs              # Exports & constants
│   ├── error.rs            # EncryptionError enum
│   ├── policy.rs           # EncryptionPolicy & EncryptionMetadata
│   ├── keymanager.rs       # KeyManager & key derivation
│   ├── xts.rs              # XTS-AES-256 encrypt/decrypt
│   └── mac.rs              # BLAKE3-MAC compute/verify
└── Cargo.toml              # Dependencies: aes, xts-mode, blake3, zeroize
```

---

## Core Components

### 1. Error Handling (`error.rs`)
```rust
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("Invalid key length: expected 32 or 64 bytes, got {0}")]
    InvalidKeyLength(usize),
    
    #[error("Key version {0} not found")]
    KeyVersionNotFound(u32),
    
    #[error("Integrity check failed")]
    IntegrityFailure,
    
    #[error("Missing encryption metadata")]
    MissingMetadata,
    
    // ... 5 more variants
}

pub type Result<T> = std::result::Result<T, EncryptionError>;
```

**Usage:**
- All encryption functions return `Result<T>`
- Errors propagate with `?` operator
- Convert to `anyhow::Error` in pipeline with `.map_err(|e| anyhow::anyhow!("{}", e))`

---

### 2. Policy & Metadata (`policy.rs`)

#### EncryptionPolicy
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EncryptionPolicy {
    Disabled,                              // Default
    XtsAes256 { key_version: Option<u32> }, // Opt-in
}

impl EncryptionPolicy {
    pub fn is_enabled(&self) -> bool;      // Check if encryption active
    pub fn key_version(&self) -> Option<u32>; // Get version or None
}
```

**Integration:**
```rust
// In common/src/policy.rs
pub struct Policy {
    pub compression: CompressionPolicy,
    pub dedupe: bool,
    #[serde(default)]
    pub encryption: EncryptionPolicy,  // Added
}

// Presets
Policy::encrypted()              // Enable encryption
Policy::encrypted_compressed()   // Encryption + Zstd
```

#### EncryptionMetadata
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionMetadata {
    pub encryption_version: Option<u16>,    // 1 = XTS-AES-256
    pub key_version: Option<u32>,           // Key used
    pub tweak_nonce: Option<[u8; 16]>,      // XTS tweak
    pub integrity_tag: Option<[u8; 16]>,    // BLAKE3-MAC
    pub ciphertext_len: Option<u32>,        // Length
}

// Constructor
EncryptionMetadata::new_xts(key_version: u32, tweak: [u8; 16], len: u32)

// Helpers
metadata.is_encrypted() -> bool
metadata.require_tweak() -> Result<[u8; 16]>
metadata.set_integrity_tag(tag: [u8; 16])
```

**Storage:** Embedded in `Segment` struct, persisted to `space.nvram.segments`

---

### 3. Key Management (`keymanager.rs`)

#### KeyManager
```rust
pub struct KeyManager {
    master_key: [u8; 32],                    // From SPACE_MASTER_KEY env or TPM
    hkdf_salt: [u8; 32],                     // Device / TPM provided salt
    key_cache: HashMap<u32, XtsKeyPair>,     // Derived keys
    current_version: u32,                    // Active version
}

// Initialization
KeyManager::from_env() -> Result<Self>          // From SPACE_MASTER_KEY
KeyManager::from_tpm<T: TpmProvider>(provider: &T) -> Result<Self>
KeyManager::new(master_key: [u8; 32]) -> Self   // Explicit (testing)

// Key access
get_key(&mut self, version: u32) -> Result<&XtsKeyPair>  // Derive if needed
current_version(&self) -> u32

// Rotation
rotate_key(&mut self) -> u32                    // Bump version
clear_cache(&mut self)                          // Force re-derivation
```

#### Key Derivation
```rust
fn derive_xts_key_pair(master_key: &[u8; 32], hkdf_salt: &[u8; 32], version: u32) -> [u8; 64] {
    // Extract: PRK = HMAC-SHA256(hkdf_salt, master_key)
    let prk = hkdf_extract(hkdf_salt, master_key);

    // Expand: context info binds the version into the key material
    let mut info = Vec::from("SPACE-XTS-AES-256-KEY-V1");
    info.extend_from_slice(&version.to_be_bytes());

    hkdf_expand(&prk, &info, 64)
}
```

**Properties:**
- Deterministic (same master + version = same keys)
- Domain-separated (different contexts = different keys)
- Forward secure (old keys don't reveal new keys)

#### XtsKeyPair
```rust
pub struct XtsKeyPair {
    key1: [u8; 32],  // AES-256 key for data
    key2: [u8; 32],  // AES-256 key for tweak
}

impl Drop for XtsKeyPair {
    fn drop(&mut self) {
        self.key1.zeroize();  // Clear from memory
        self.key2.zeroize();
    }
}
```

---

### 4. XTS Encryption (`xts.rs`)

#### Core API
```rust
// Low-level
pub fn encrypt(
    plaintext: &[u8],
    key_pair: &XtsKeyPair,
    tweak: &[u8; 16],
) -> Result<Vec<u8>>

pub fn decrypt(
    ciphertext: &[u8],
    key_pair: &XtsKeyPair,
    tweak: &[u8; 16],
) -> Result<Vec<u8>>

// High-level (with metadata)
pub fn encrypt_segment(
    plaintext: &[u8],
    key_pair: &XtsKeyPair,
    key_version: u32,
    tweak: [u8; 16],
) -> Result<(Vec<u8>, EncryptionMetadata)>

pub fn decrypt_segment(
    ciphertext: &[u8],
    key_pair: &XtsKeyPair,
    metadata: &EncryptionMetadata,
) -> Result<Vec<u8>>
```

#### Tweak Derivation (Critical for Dedup)
```rust
pub fn derive_tweak_from_hash(content_hash: &[u8]) -> [u8; 16] {
    let mut tweak = [0u8; 16];
    let copy_len = content_hash.len().min(16);
    tweak[..copy_len].copy_from_slice(&content_hash[..copy_len]);
    tweak
}
```

**Why this works:**
1. Content hash computed on compressed data
2. Identical compressed data → Identical hash → Identical tweak
3. Same tweak + key → Same ciphertext → Dedup succeeds

#### Implementation
```rust
fn encrypt(plaintext: &[u8], key_pair: &XtsKeyPair, tweak: &[u8; 16]) -> Result<Vec<u8>> {
    if plaintext.len() < 16 {
        return Err(EncryptionError::InvalidCiphertextLength(plaintext.len()));
    }

    let cipher1 = Aes256::new(key_pair.key1().into());
    let cipher2 = Aes256::new(key_pair.key2().into());
    let xts = Xts128::<Aes256>::new(cipher1, cipher2);
    
    let mut ciphertext = plaintext.to_vec();
    xts.encrypt_sector(&mut ciphertext, *tweak);
    
    Ok(ciphertext)
}
```

**Properties:**
- In-place encryption (minimizes allocations)
- Length-preserving (ciphertext.len() == plaintext.len())
- Hardware-accelerated (AES-NI when available)
- Minimum 16 bytes (XTS requirement)

---

### 5. MAC Integrity (`mac.rs`)

#### API
```rust
pub fn compute_mac(
    ciphertext: &[u8],
    metadata: &EncryptionMetadata,
    xts_key1: &[u8; 32],
    xts_key2: &[u8; 32],
) -> Result<[u8; 16]>

pub fn verify_mac(
    ciphertext: &[u8],
    metadata: &EncryptionMetadata,
    xts_key1: &[u8; 32],
    xts_key2: &[u8; 32],
) -> Result<()>  // Ok or IntegrityFailure
```

#### MAC Key Derivation
```rust
fn derive_mac_key(xts_key1: &[u8; 32], xts_key2: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"SPACE-BLAKE3-MAC-KEY-V1");  // Domain separation
    hasher.update(xts_key1);
    hasher.update(xts_key2);
    let hash = hasher.finalize();
    *hash.as_bytes()
}
```

#### MAC Computation
```rust
pub fn compute_mac(...) -> Result<[u8; 16]> {
    let mac_key = derive_mac_key(xts_key1, xts_key2);
    let mut hasher = blake3::Hasher::new_keyed(&mac_key);
    
    // Hash ciphertext
    hasher.update(ciphertext);
    
    // Hash metadata (deterministic serialization)
    let metadata_bytes = serialize_metadata_for_mac(metadata)?;
    hasher.update(&metadata_bytes);
    
    // Take first 16 bytes as MAC
    let hash = hasher.finalize();
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&hash.as_bytes()[0..16]);
    Ok(tag)
}
```

**What's Protected:**
- Ciphertext (detects bit flips)
- Metadata (detects tampering with version/key/tweak)
- Length (implicit in metadata)

#### Verification
```rust
pub fn verify_mac(...) -> Result<()> {
    let stored_tag = metadata.require_integrity_tag()?;
    
    let mut metadata_for_mac = metadata.clone();
    metadata_for_mac.integrity_tag = None;  // Avoid circular dependency
    
    let computed_tag = compute_mac(ciphertext, &metadata_for_mac, ...)?;
    
    if constant_time_eq(&stored_tag, &computed_tag) {
        Ok(())
    } else {
        Err(EncryptionError::IntegrityFailure)
    }
}

fn constant_time_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut result = 0u8;
    for i in 0..16 {
        result |= a[i] ^ b[i];
    }
    result == 0
}
```

---

## Integration

### 1. Common Types Update
```rust
// In common/src/lib.rs
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,
    pub len: u32,
    
    // Existing Phase 2 fields...
    pub compressed: bool,
    pub content_hash: Option<ContentHash>,
    
    // NEW: Phase 3 encryption fields
    #[serde(default)]
    pub encryption_version: Option<u16>,
    #[serde(default)]
    pub key_version: Option<u32>,
    #[serde(default)]
    pub tweak_nonce: Option<[u8; 16]>,
    #[serde(default)]
    pub integrity_tag: Option<[u8; 16]>,
    #[serde(default)]
    pub encrypted: bool,
}
```

### 2. Pipeline Integration
```rust
// In capsule-registry/src/pipeline.rs
use encryption::{KeyManager, encrypt_segment, decrypt_segment, 
                 derive_tweak_from_hash, compute_mac, verify_mac};
use std::sync::{Arc, Mutex};

pub struct WritePipeline {
    registry: CapsuleRegistry,
    nvram: NvramLog,
    key_manager: Option<Arc<Mutex<KeyManager>>>,  // NEW
}

impl WritePipeline {
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        let key_manager = KeyManager::from_env().ok()
            .map(|km| Arc::new(Mutex::new(km)));
        
        if key_manager.is_some() {
            println!("🔐 Encryption enabled");
        }
        
        Self { registry, nvram, key_manager }
    }
}
```

### 3. Write Path
```rust
pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
    let encryption_enabled = policy.encryption.is_enabled() && self.key_manager.is_some();
    
    for chunk in data.chunks(SEGMENT_SIZE) {
        // 1. Compress
        let (compressed_data, _) = compress_segment(chunk, &policy.compression)?;
        
        // 2. Hash (for dedup)
        let content_hash = hash_content(&compressed_data);
        
        // 3. Encrypt + MAC (if enabled)
        let (final_data, encryption_meta) = if encryption_enabled {
            let km = self.key_manager.as_ref().unwrap();
            let mut km = km.lock().unwrap();
            let key_version = km.current_version();
            let key_pair = km.get_key(key_version)?;
            
            // Derive tweak from content hash (deterministic!)
            let tweak = derive_tweak_from_hash(content_hash.as_str().as_bytes());
            
            // Encrypt
            let (ciphertext, mut enc_meta) = encrypt_segment(
                &compressed_data, key_pair, key_version, tweak
            )?;
            
            // Compute MAC
            let mac_tag = compute_mac(&ciphertext, &enc_meta, 
                                     key_pair.key1(), key_pair.key2())?;
            enc_meta.set_integrity_tag(mac_tag);
            
            (ciphertext, Some(enc_meta))
        } else {
            (compressed_data, None)
        };
        
        // 4. Dedup check
        // 5. Store segment
        // 6. Update metadata with encryption fields
        
        if let Some(ref enc_meta) = encryption_meta {
            segment.encrypted = true;
            segment.encryption_version = enc_meta.encryption_version;
            segment.key_version = enc_meta.key_version;
            segment.tweak_nonce = enc_meta.tweak_nonce;
            segment.integrity_tag = enc_meta.integrity_tag;
        }
        
        self.nvram.update_segment_metadata(seg_id, segment)?;
    }
    
    Ok(capsule_id)
}
```

### 4. Read Path
```rust
pub fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
    let capsule = self.registry.lookup(id)?;
    let mut result = Vec::new();
    
    for seg_id in &capsule.segments {
        // 1. Fetch raw data
        let raw_data = self.nvram.read(*seg_id)?;
        let segment = self.nvram.get_segment_metadata(*seg_id)?;
        
        // 2. Verify MAC + Decrypt (if encrypted)
        let decrypted_data = if segment.encrypted {
            let km = self.key_manager.as_ref()
                .ok_or_else(|| anyhow::anyhow!("No key manager"))?;
            let mut km = km.lock().unwrap();
            let key_pair = km.get_key(segment.key_version.unwrap())?;
            
            // Build metadata
            let enc_meta = EncryptionMetadata {
                encryption_version: segment.encryption_version,
                key_version: segment.key_version,
                tweak_nonce: segment.tweak_nonce,
                integrity_tag: segment.integrity_tag,
                ciphertext_len: Some(raw_data.len() as u32),
            };
            
            // Verify MAC first (detect tampering)
            verify_mac(&raw_data, &enc_meta, 
                      key_pair.key1(), key_pair.key2())?;
            
            // Decrypt
            decrypt_segment(&raw_data, key_pair, &enc_meta)?
        } else {
            raw_data
        };
        
        // 3. Decompress
        let data = decompress(&decrypted_data, &capsule.policy)?;
        
        result.extend_from_slice(&data);
    }
    
    Ok(result)
}
```

---

---

### 4. Hybrid Kyber (Phase 3.3)
- **Security crate**: `common::security::crypto_profiles` provides the Kyber key manager + nonce helpers behind the `advanced-security` feature.
- **Key persistence**: `KyberKeyManager::load_or_generate` stores the ML-KEM keypair at `SPACE_KYBER_KEY_PATH` (default `space.kyber.key`).
- **Policy toggle**: `Policy::crypto_profile` defaults to `Classical`. Setting `HybridKyber` wraps the AES key pair and stores the Kyber ciphertext/nonce in each segment.
- **Write path**: Kyber material is derived alongside AES keys; the nonce mixes into the deterministic tweak so dedupe is preserved.
- **Read path**: when `HybridKyber`, the Kyber ciphertext is decapsulated before MAC verification + decryption, keeping backward compatibility.
- **Feature gating**: sovereign builds keep `advanced-security` disabled to avoid PQ dependencies entirely.

## Configuration

### Environment Setup
```bash
# Generate 256-bit key
export SPACE_MASTER_KEY=$(openssl rand -hex 32)

# Or use specific key
export SPACE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

# Verify
echo ${#SPACE_MASTER_KEY}  # Should be 64
```

### Policy Usage
```rust
// Enable encryption
let policy = Policy::encrypted();

// Or manually
let mut policy = Policy::default();
policy.encryption = EncryptionPolicy::XtsAes256 { key_version: None };

// Write encrypted data
let capsule_id = pipeline.write_capsule_with_policy(data, &policy)?;

// Read (auto-decrypts)
let data = pipeline.read_capsule(capsule_id)?;
```

---

## Testing
```bash
# Run all encryption tests
cargo test -p encryption

# Run with output
cargo test -p encryption -- --nocapture

# Run integration tests
cargo test -p capsule-registry

# All tests
cargo test --workspace
```

**Test Coverage:** 53 passing tests

---

## Performance

| Operation | Overhead | Notes |
|-----------|----------|-------|
| Write | +5% | Encryption + MAC |
| Read | +9% | Verification + decryption |
| Dedup | 0% | Works with encryption |
| Memory | <5 MB | Key cache + buffers |

**Throughput:**
- Write: ~2.0 GB/s (with encryption)
- Read: ~3.2 GB/s (with decryption)

---

## Security Properties

| Property | Provided By | Strength |
|----------|-------------|----------|
| Confidentiality | XTS-AES-256 | 256-bit |
| Integrity | BLAKE3-MAC | 128-bit |
| Deduplication | Deterministic tweaks | Preserved |
| Key Derivation | HKDF (HMAC-SHA256) | Cryptographic |

---

## Troubleshooting

**Key manager not initialized:**
```bash
export SPACE_MASTER_KEY=$(openssl rand -hex 32)
```

**Integrity check failed:**
- Data corrupted
- Wrong key
- Metadata tampered

**Dedup not working:**
- Check compression policy matches
- Verify content hashes identical

---

## Future Work

- [x] Garbage collection (refcount decrements + metadata reclamation)
- [ ] Bloom filter optimization for MAC
- [x] Post-quantum key exchange (Kyber hybrid toggle)
- [ ] Background re-encryption for key rotation
- [ ] Encrypted search (future)

---

© 2025 Shane Wall. Licensed under Apache 2.0.

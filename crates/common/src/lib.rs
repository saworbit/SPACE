use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "advanced-security")]
pub mod security;

pub mod policy;
pub mod traits;
pub use policy::{CompressionPolicy, CryptoProfile, EncryptionPolicy, Policy};

pub const SEGMENT_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapsuleId(pub Uuid);

impl CapsuleId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for CapsuleId {
    fn default() -> Self {
        Self::new()
    }
}

// NEW: Content-addressable hash for deduplication
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn from_bytes(hash: &[u8]) -> Self {
        Self(hex::encode(hash))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capsule {
    pub id: CapsuleId,
    pub size: u64,
    pub segments: Vec<SegmentId>,
    pub created_at: u64,

    #[serde(default)]
    pub policy: Policy,

    // Phase 2.2: Track dedup stats per capsule
    #[serde(default)]
    pub deduped_bytes: u64, // How many bytes were deduplicated
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,
    pub len: u32,

    // Phase 2.1: Compression metadata
    #[serde(default)]
    pub compressed: bool,
    #[serde(default)]
    pub compression_algo: String,

    // Phase 2.2: Deduplication metadata
    #[serde(default)]
    pub content_hash: Option<ContentHash>, // Hash of compressed data
    #[serde(default)]
    pub ref_count: u32, // Reference count for GC

    #[serde(default)]
    pub deduplicated: bool,
    #[serde(default)]
    pub access_count: u32,

    // Phase 3: Encryption metadata
    #[serde(default)]
    pub encryption_version: Option<u16>, // Encryption format version
    #[serde(default)]
    pub key_version: Option<u32>, // Key version used
    #[serde(default)]
    pub tweak_nonce: Option<[u8; 16]>, // XTS tweak
    #[serde(default)]
    pub integrity_tag: Option<[u8; 16]>, // MAC tag
    #[serde(default)]
    pub encrypted: bool, // Quick check if encrypted

    // Phase 3.3: Post-quantum metadata
    #[serde(default)]
    pub pq_ciphertext: Option<String>,
    #[serde(default)]
    pub pq_nonce: Option<[u8; 16]>,
}

/// Immutable audit log events emitted by the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    CapsuleCreated {
        capsule_id: CapsuleId,
        size: u64,
        segments: usize,
        policy: Policy,
    },
    CapsuleRead {
        capsule_id: CapsuleId,
        size: u64,
    },
    CapsuleDeleted {
        capsule_id: CapsuleId,
        reclaimed_bytes: u64,
    },
    SegmentAppended {
        segment_id: SegmentId,
        len: u32,
        content_hash: Option<ContentHash>,
        encrypted: bool,
    },
    DedupHit {
        segment_id: SegmentId,
        capsule_id: CapsuleId,
        content_hash: ContentHash,
    },
    AuditHeartbeat {
        timestamp: u64,
        capsules: usize,
        segments: usize,
    },
}

//! Shared types, policies, and traits for the SPACE storage platform.
//!
//! This crate contains the foundational types used across all SPACE
//! subsystems:
//!
//! - **Core types**: `CapsuleId`, `SegmentId`, `Capsule`, `Segment`, `ContentHash`
//! - **Policy**: `Policy`, `EncryptionPolicy`, `CompressionPolicy`, and related types
//! - **Traits**: `Compressor`, `Encryptor`, `Deduper`, `StorageBackend`, etc.
//! - **Erasure coding**: `ErasureCode` trait, `ErasureProfile`, shard types
//! - **Scrub scheduler**: `ScrubConfig`, `ScrubSchedule`, `ScrubResult`
//! - **QoS admission control**: `QosScheduler`, `IoClass`, `QosPermit`

use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use uuid::Uuid;

pub mod erasure;
pub mod policy;
pub mod qos;
pub mod scrub;
pub mod stub;
pub mod traits;
pub use policy::{
    ArtifactVerification, CompressionPolicy, CryptoProfile, EncryptionPolicy, FederationPolicy,
    FederationStrategy, LayoutPolicy, LayoutStrategy, MerkleAlgo, Policy, ResourceLimits,
    TransferPriority, TransformDef, TransformTrigger,
};
pub use stub::StorageStub;

/// Default segment size: 4 MiB.
///
/// Chosen as the empirical sweet spot for SPACE workloads: large enough to
/// amortize per-segment metadata (~120 bytes) to under 0.01% overhead, small
/// enough for fine-grained range reads and frequent dedup hits, and aligned
/// with common SSD erase blocks and io_uring submission sizes. The
/// `LayoutPolicy` can override this with adaptive/learned strategies; this
/// constant is the fallback when no smarter strategy is configured.
pub const SEGMENT_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Identifier for a stored segment within a single storage backend.
///
/// Segments are the unit of dedup, compression, and encryption. A `Capsule`
/// holds an ordered `Vec<SegmentId>`; reads walk that list in order. Segment
/// IDs are monotonic within a backend and never repeat (the backend is
/// append-only). Different backends have independent sequence spaces.
///
/// 64 bits is wide enough that no plausible deployment will exhaust the
/// sequence within a single log; narrower would force ID recycling and
/// complicate GC.
///
/// See [`Segment`] for the metadata record this ID keys into, and
/// `docs/capsule.md` for the full lifecycle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct SegmentId(pub u64);

/// 128-bit identity of a Capsule.
///
/// A Capsule is the foundational unit of storage in SPACE — everything else
/// (protocols, replication, federation, scrub, tiering) operates on Capsules.
/// `CapsuleId` is a UUID v4 generated without coordination, so writes can
/// proceed at any node — including air-gapped edge sites — without contacting
/// a central authority.
///
/// **Identity vs. representation.** A `CapsuleId` is stable for the
/// Capsule's lifetime. Storage-layer transformations (PODMS swarm migration,
/// key rotation, compression transcoding, tier promotion/demotion) rewrite
/// the on-disk segments while preserving the `CapsuleId` and the logical
/// content that `read()` returns. Protocol-layer mutations (overwriting a
/// file at the NFS view, rewriting a region at the block view, PUT-ing a new
/// object body) produce a *new* Capsule with a *new* `CapsuleId`; the view
/// updates its metadata to point at the new one. See `docs/capsule.md` §2.1
/// for the full operation × identity matrix.
///
/// `shard_keys(count)` derives a deterministic set of shard keys for
/// distributing capsule metadata across registry shards. The same UUID
/// always maps to the same shards; required for read locality across
/// replicas.
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

    /// Derive deterministic shard keys for metadata distribution.
    pub fn shard_keys(&self, count: usize) -> Vec<u64> {
        let count = count.max(1);
        let bytes = self.as_uuid().as_bytes();
        let base = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or_default());
        (0..count)
            .map(|idx| base.wrapping_add(idx as u64))
            .collect()
    }
}

impl Default for CapsuleId {
    fn default() -> Self {
        Self::new()
    }
}

/// Content-addressable hash of a segment payload.
///
/// Stored as a hex string for stable serialization across versions. Computed
/// by `dedup::hash_content_with_algo(compressed_bytes, compression_algo)` over
/// the **compressed, pre-encryption** bytes, with the compression algorithm
/// mixed into the BLAKE3 input as a domain separator.
///
/// The hashing point in the pipeline is load-bearing:
/// - Hashing the *plaintext* would break dedup across compression policies.
/// - Hashing the *ciphertext* would require content-independent tweaks,
///   leaking content or breaking dedup.
/// - Hashing the *compressed pre-encryption* bytes is the unique point where
///   dedup, deterministic tweak derivation, and encryption-preserving dedup
///   all align. See `docs/capsule.md` §10.5.
///
/// The algorithm domain separator is critical: without it, raw LZ4-framed
/// bytes stored under `CompressionPolicy::None` would collide with the same
/// frame produced by compressing plaintext under `CompressionPolicy::LZ4`.
/// See `docs/capsule.md` §10.4 and the
/// `prop_no_cross_policy_dedup_collision` proptest.
///
/// Use `hash_content_with_algo`, never bare `hash_content`, on the write
/// path. The bare variant exists only for the legacy verification fallback.
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

/// A content-addressed, policy-bound collection of segments with stable identity.
///
/// A Capsule is **the** durable primitive in SPACE. All higher-level objects
/// — files, blocks, S3 objects, NVMe namespaces — are *views* projected onto
/// Capsules; none of them own bytes.
///
/// # Identity vs. representation
///
/// - `CapsuleId` and the logical content returned by `read()` are stable for
///   the Capsule's lifetime.
/// - The segment list, per-segment content hashes, encryption metadata, and
///   the `policy` field **can change in place** when the Capsule is
///   transformed by storage-layer operations: PODMS swarm migration, key
///   rotation, compression transcoding, tier promotion/demotion.
/// - Protocol-layer mutations (overwriting a file at the NFS view, rewriting
///   a region at the block view, PUT-ing a new object body) produce a *new*
///   Capsule with a *new* `CapsuleId`; the view's metadata is updated to
///   point at the new one. The Capsule abstraction is not in-place mutable
///   from the protocol surface.
///
/// See `docs/capsule.md` §2.1 for the full operation × identity matrix.
///
/// # Read-path invariant
///
/// When reading a Capsule's bytes, **trust segment metadata, not policy**.
/// Branch on `segment.compressed`, `segment.encrypted`, and
/// `segment.compression_algo` — never on `capsule.policy`. The policy
/// reflects the *current* representation but segments may be heterogeneous
/// mid-transformation; segment fields are the durable truth.
///
/// See `docs/capsule.md` for the full lifecycle, write/read flow, and
/// rationale behind each field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capsule {
    /// 128-bit identity. See [`CapsuleId`].
    pub id: CapsuleId,

    /// Logical (uncompressed, decrypted) byte count. Required for range reads
    /// without materializing segments, and for reporting `df`/`ls -l`
    /// semantics through views.
    pub size: u64,

    /// Ordered list of segments. Reads walk this list; order = logical byte
    /// order.
    pub segments: Vec<SegmentId>,

    /// Unix-seconds creation timestamp. Used by heat/age policies and audit.
    pub created_at: u64,

    /// The compression/encryption/layout/federation/transform policy
    /// **currently active** for this Capsule. Updated in place by
    /// storage-layer transformations (PODMS migration, key rotation,
    /// compression transcoding); see `docs/capsule.md` §2.1.
    ///
    /// Read-path code must **not** branch on this field — segments may be
    /// heterogeneous during transformation. Branch on `Segment.encrypted`,
    /// `Segment.compressed`, and `Segment.compression_algo` instead.
    #[serde(default)]
    pub policy: Policy,

    /// Bytes that hit existing segments during write (dedup savings for this
    /// Capsule). Operator visibility into per-Capsule dedup effectiveness.
    #[serde(default)]
    pub deduped_bytes: u64,
}

/// Metadata record for one stored segment.
///
/// A Segment is the unit of dedup, compression, and encryption. Default size
/// is `SEGMENT_SIZE` (4 MiB), overridable by `LayoutPolicy`. Multiple Capsules
/// may reference the same Segment via dedup; `ref_count` tracks references
/// and gates GC.
///
/// # Field semantics
///
/// Segment fields are the **durable truth** about how the bytes are stored.
/// The read path branches on these fields, not on `Capsule.policy`:
///
/// - `compressed` / `compression_algo` → which decompressor to invoke
/// - `encrypted` / `key_version` / `tweak_nonce` / `integrity_tag` → how to
///   decrypt and verify
/// - `content_hash` → what BLAKE3 hash to expect on deep scrub
///
/// See `docs/capsule.md` §3.5 for the field-by-field rationale.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Segment {
    /// Monotonic ID within the backend. See [`SegmentId`].
    pub id: SegmentId,
    /// Byte offset in the backend's segment log.
    pub offset: u64,
    /// Length of stored bytes (post-compression, post-encryption).
    pub len: u32,

    /// Logical length of the segment payload after decompression/decryption.
    /// Used for range-aware reads to skip pre-range segments without decoding.
    #[serde(default)]
    pub plain_len: Option<u32>,

    /// Whether stored bytes are compressed. The read path checks this field,
    /// not `Capsule.policy.compression`.
    #[serde(default)]
    pub compressed: bool,
    /// Compression algorithm string (e.g. `"identity"`, `"lz4:1"`,
    /// `"zstd:3"`). Mixed into `content_hash` for domain separation; used by
    /// the read path to dispatch to the correct decompressor.
    #[serde(default)]
    pub compression_algo: String,

    /// BLAKE3 hash of the stored (compressed, pre-encryption) bytes, with the
    /// compression algorithm mixed in. Key for the dedup index and for deep
    /// scrub verification of unencrypted segments.
    #[serde(default)]
    pub content_hash: Option<ContentHash>,
    /// Number of Capsules referencing this segment. Incremented on dedup hit,
    /// decremented on Capsule delete. Eligible for GC when zero.
    #[serde(default)]
    pub ref_count: u32,

    /// True if at least one dedup hit has occurred against this segment.
    /// Operator visibility flag.
    #[serde(default)]
    pub deduplicated: bool,
    /// Cumulative read count. Drives the tiering heatmap.
    #[serde(default)]
    pub access_count: u32,

    /// Encryption format version. Allows future migration to a new
    /// encryption envelope.
    #[serde(default)]
    pub encryption_version: Option<u16>,
    /// Key version used to encrypt this segment. The read path passes this
    /// to the key manager to select the correct `XtsKeyPair`.
    #[serde(default)]
    pub key_version: Option<u32>,
    /// 16-byte XTS tweak. Derived from the first 16 bytes of `content_hash`,
    /// making encryption deterministic and dedup-preserving. See
    /// `docs/capsule.md` §10.2.
    #[serde(default)]
    pub tweak_nonce: Option<[u8; 16]>,
    /// 16-byte BLAKE3-MAC tag covering `ciphertext || encryption_metadata`.
    /// Verified before decryption (MAC-then-decrypt). See
    /// `docs/capsule.md` §10.3.
    #[serde(default)]
    pub integrity_tag: Option<[u8; 16]>,
    /// Quick check for whether bytes are encrypted. The read path branches
    /// on this, not `Capsule.policy.encryption`.
    #[serde(default)]
    pub encrypted: bool,

    /// Post-quantum hybrid-wrap ciphertext (gated, Phase 3.3).
    #[serde(default)]
    pub pq_ciphertext: Option<String>,
    /// Post-quantum hybrid-wrap nonce (gated, Phase 3.3).
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



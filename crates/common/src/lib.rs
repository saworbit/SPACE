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

#[cfg(feature = "advanced-security")]
pub mod security;

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

// ============================================================================
// PODMS (Policy-Orchestrated Disaggregated Mesh Scaling) Types
// ============================================================================
// These types enable distributed mesh scaling while maintaining single-node
// compatibility. All PODMS features are gated behind the "podms" feature flag.

#[cfg(feature = "podms")]
pub mod podms {
    use super::*;
    use std::time::Duration;

    /// Unique identifier for a node in the SPACE mesh.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct NodeId(pub Uuid);

    impl NodeId {
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

    impl Default for NodeId {
        fn default() -> Self {
            Self::new()
        }
    }

    impl std::fmt::Display for NodeId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    /// Zone identifier for data sovereignty and placement control.
    /// Supports metro (availability zone), geo (region), and edge deployments.
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ZoneId {
        /// Metro zone (e.g., "us-west-1a")
        Metro { name: String },
        /// Geographic region (e.g., "eu-central")
        Geo { name: String },
        /// Edge site (e.g., "air-gapped-site-42")
        Edge { name: String },
    }

    impl std::fmt::Display for ZoneId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ZoneId::Metro { name } => write!(f, "metro:{}", name),
                ZoneId::Geo { name } => write!(f, "geo:{}", name),
                ZoneId::Edge { name } => write!(f, "edge:{}", name),
            }
        }
    }

    /// Data sovereignty level controlling replication scope.
    /// Determines where data can be replicated and migrated.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    #[serde(rename_all = "snake_case")]
    pub enum SovereigntyLevel {
        /// No external replication - data stays on local node
        #[default]
        Local,
        /// Replication within defined zones only
        Zone,
        /// Full federation across all zones
        Global,
    }

    /// Telemetry events for PODMS autonomous agents.
    /// These events signal state changes that may trigger scaling actions.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum Telemetry {
        /// New capsule created - may trigger replication
        NewCapsule {
            id: CapsuleId,
            policy: Policy,
            node_id: Option<NodeId>,
        },
        /// Heat spike detected - may trigger migration
        HeatSpike {
            id: CapsuleId,
            accesses_per_min: u64,
            node_id: Option<NodeId>,
        },
        /// Capacity threshold reached - may trigger balancing
        CapacityThreshold {
            node_id: NodeId,
            used_bytes: u64,
            total_bytes: u64,
            threshold_pct: f64,
        },
        /// Node health degraded - may trigger evacuation
        NodeDegraded { node_id: NodeId, reason: String },
        /// Request to project a capsule into a protocol-specific view.
        ViewProjection { id: CapsuleId, view: String },
        /// Operator-driven trigger to force RPO policy execution immediately.
        ForcePolicyExecution {
            capsule_id: CapsuleId,
            /// Override policy RPO for this invocation (None = use capsule policy)
            forced_rpo: Option<Duration>,
        },
    }

    /// Interface for crypto/compression operations provided by the runtime.
    /// This resolves the circular dependency between `common` and
    /// higher-level crypto/compression crates by pushing the concrete
    /// implementation to callers.
    pub trait TransformOps {
        /// Decrypt data using the provided policy and segment context (for tweaks).
        /// `capsule_id` provides the context needed for per-capsule key derivation.
        fn decrypt(
            &self,
            capsule_id: CapsuleId,
            data: &[u8],
            policy: &EncryptionPolicy,
            ctx: SegmentId,
        ) -> anyhow::Result<Vec<u8>>;

        /// Encrypt data using the provided policy and segment context.
        /// `capsule_id` provides the context needed for per-capsule key derivation.
        fn encrypt(
            &self,
            capsule_id: CapsuleId,
            data: &[u8],
            policy: &EncryptionPolicy,
            ctx: SegmentId,
        ) -> anyhow::Result<Vec<u8>>;

        /// Decompress data.
        fn decompress(&self, data: &[u8], policy: &CompressionPolicy) -> anyhow::Result<Vec<u8>>;

        /// Compress data.
        fn compress(&self, data: &[u8], policy: &CompressionPolicy) -> anyhow::Result<Vec<u8>>;
    }

    /// Swarm behavior trait for capsule self-transformation during migrations.
    ///
    /// This trait enables PODMS "swarm intelligence" where capsules autonomously
    /// adapt their representation (compression, encryption) based on policy
    /// constraints during migration or replication events.
    pub trait SwarmBehavior {
        /// Apply policy-driven transformation to capsule data.
        ///
        /// Transforms follow an unwrap → transcode → rewrap sequence to
        /// preserve security while adapting to new placement contexts.
        fn apply_transform<T: TransformOps>(
            &self,
            segment_id: SegmentId,
            data: &[u8],
            target_policy: &Policy,
            ops: &T,
        ) -> anyhow::Result<Vec<u8>>;

        /// Hook called before migration to validate and prepare.
        fn on_migrate(&self, destination: NodeId, dest_zone: &ZoneId) -> anyhow::Result<()>;

        /// Determine if transformation is required for migration.
        fn requires_transform(&self, source_zone: &ZoneId, dest_zone: &ZoneId) -> bool;
    }

    /// Implementation of SwarmBehavior for Capsule.
    impl SwarmBehavior for Capsule {
        fn apply_transform<T: TransformOps>(
            &self,
            segment_id: SegmentId,
            data: &[u8],
            target_policy: &Policy,
            ops: &T,
        ) -> anyhow::Result<Vec<u8>> {
            let src_enc = &self.policy.encryption;
            let dst_enc = &target_policy.encryption;
            let src_comp = &self.policy.compression;
            let dst_comp = &target_policy.compression;

            // Unwrap: decrypt if the source policy enabled encryption.
            let mut payload = if self.is_encrypted() {
                ops.decrypt(self.id, data, src_enc, segment_id)?
            } else {
                data.to_vec()
            };

            // Transcode: only decompress/re-compress when compression policies differ.
            let compression_changed = src_comp != dst_comp;

            if self.is_compressed() && compression_changed {
                payload = ops.decompress(&payload, src_comp)?;
            }

            if compression_changed && !matches!(dst_comp, CompressionPolicy::None) {
                payload = ops.compress(&payload, dst_comp)?;
            }

            // Rewrap: always honor the target encryption policy (re-encrypt to rotate keys).
            if !matches!(dst_enc, EncryptionPolicy::Disabled) {
                payload = ops.encrypt(self.id, &payload, dst_enc, segment_id)?;
            }

            Ok(payload)
        }

        fn on_migrate(&self, destination: NodeId, dest_zone: &ZoneId) -> anyhow::Result<()> {
            // Validate sovereignty constraints
            match self.policy.sovereignty {
                SovereigntyLevel::Local => {
                    return Err(anyhow::anyhow!(
                        "SOVEREIGNTY VIOLATION: Capsule {:?} is restricted to Local scope. Migration to node {} in zone {} denied.",
                        self.id, destination, dest_zone
                    ));
                }
                SovereigntyLevel::Zone => {
                    tracing::debug!(
                        capsule_id = ?self.id,
                        destination = %destination,
                        dest_zone = %dest_zone,
                        "validating zone sovereignty for migration"
                    );
                }
                SovereigntyLevel::Global => {
                    // No restrictions
                }
            }

            Ok(())
        }

        fn requires_transform(&self, source_zone: &ZoneId, dest_zone: &ZoneId) -> bool {
            // Transformation needed if:
            // 1. Crossing zone boundaries (for re-encryption)
            // 2. Policy change (e.g., moving to cold storage)
            source_zone != dest_zone
        }
    }

    // Helper methods for Capsule (PODMS-specific)
    impl Capsule {
        /// Check if capsule data is encrypted based on policy.
        pub fn is_encrypted(&self) -> bool {
            self.policy.encryption.is_enabled()
        }

        /// Check if capsule data is compressed based on policy.
        pub fn is_compressed(&self) -> bool {
            !matches!(self.policy.compression, CompressionPolicy::None)
        }

        /// Determine if capsule should be treated as cold data.
        ///
        /// Cold data has low access frequency and benefits from higher compression.
        #[allow(dead_code)]
        pub fn is_cold_data(&self) -> bool {
            // Placeholder heuristic: requires richer telemetry to evaluate.
            let _ = self.segments.len();
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // Mock Ops for testing the logic flow without full crypto stack.
        struct MockOps;
        impl TransformOps for MockOps {
            fn decrypt(
                &self,
                _capsule_id: CapsuleId,
                data: &[u8],
                _p: &EncryptionPolicy,
                _id: SegmentId,
            ) -> anyhow::Result<Vec<u8>> {
                let mut d = data.to_vec();
                d.reverse();
                Ok(d)
            }
            fn encrypt(
                &self,
                _capsule_id: CapsuleId,
                data: &[u8],
                _p: &EncryptionPolicy,
                _id: SegmentId,
            ) -> anyhow::Result<Vec<u8>> {
                let mut d = data.to_vec();
                d.reverse();
                Ok(d)
            }
            fn decompress(&self, data: &[u8], _p: &CompressionPolicy) -> anyhow::Result<Vec<u8>> {
                Ok(data.to_vec())
            }
            fn compress(&self, data: &[u8], _p: &CompressionPolicy) -> anyhow::Result<Vec<u8>> {
                Ok(data.to_vec())
            }
        }

        #[test]
        fn test_transformation_pipeline() {
            let policy = Policy {
                encryption: EncryptionPolicy::XtsAes256 {
                    key_version: Some(1),
                },
                ..Policy::default()
            };

            let capsule = Capsule {
                id: CapsuleId::new(),
                size: 100,
                segments: vec![],
                created_at: 0,
                policy,
                deduped_bytes: 0,
            };

            let data = vec![1, 2, 3, 4];
            let segment_id = SegmentId(1);

            // decrypt (reverse) + encrypt (reverse) yields original buffer
            let res = capsule
                .apply_transform(segment_id, &data, &capsule.policy, &MockOps)
                .unwrap();
            assert_eq!(res, data);
        }

        #[test]
        fn test_sovereignty_block() {
            let policy = Policy {
                sovereignty: SovereigntyLevel::Local,
                ..Policy::default()
            };

            let capsule = Capsule {
                id: CapsuleId::new(),
                size: 0,
                segments: vec![],
                created_at: 0,
                policy,
                deduped_bytes: 0,
            };

            let dest = NodeId::new();
            let zone = ZoneId::Metro {
                name: "remote".into(),
            };

            assert!(capsule.on_migrate(dest, &zone).is_err());
        }

        #[test]
        fn test_node_id_creation() {
            let node1 = NodeId::new();
            let node2 = NodeId::new();
            assert_ne!(node1, node2);
        }

        #[test]
        fn test_node_id_from_uuid() {
            let uuid = Uuid::new_v4();
            let node_id = NodeId::from_uuid(uuid);
            assert_eq!(node_id.as_uuid(), &uuid);
        }

        #[test]
        fn test_zone_id_display() {
            let metro = ZoneId::Metro {
                name: "us-west-1a".to_string(),
            };
            let geo = ZoneId::Geo {
                name: "eu-central".to_string(),
            };
            let edge = ZoneId::Edge {
                name: "site-42".to_string(),
            };

            assert_eq!(metro.to_string(), "metro:us-west-1a");
            assert_eq!(geo.to_string(), "geo:eu-central");
            assert_eq!(edge.to_string(), "edge:site-42");
        }

        #[test]
        fn test_sovereignty_level_default() {
            let level = SovereigntyLevel::default();
            assert_eq!(level, SovereigntyLevel::Local);
        }

        #[test]
        fn test_telemetry_serialization() {
            let capsule_id = CapsuleId::new();
            let node_id = NodeId::new();
            let policy = Policy::default();

            let telemetry = Telemetry::NewCapsule {
                id: capsule_id,
                policy,
                node_id: Some(node_id),
            };

            let json = serde_json::to_string(&telemetry).unwrap();
            let deserialized: Telemetry = serde_json::from_str(&json).unwrap();

            match deserialized {
                Telemetry::NewCapsule {
                    id,
                    policy: _,
                    node_id,
                } => {
                    assert_eq!(id, capsule_id);
                    assert!(node_id.is_some());
                }
                _ => panic!("Wrong telemetry variant"),
            }
        }
    }
}

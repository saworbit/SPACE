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
    /// Wraps a UUID to represent individual storage nodes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct NodeId(pub Uuid);

    impl NodeId {
        /// Create a new random NodeId
        pub fn new() -> Self {
            Self(Uuid::new_v4())
        }

        /// Create from an existing UUID
        pub fn from_uuid(id: Uuid) -> Self {
            Self(id)
        }

        /// Get the underlying UUID
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum SovereigntyLevel {
        /// No external replication - data stays on local node
        Local,
        /// Replication within defined zones only
        Zone,
        /// Full federation across all zones
        Global,
    }

    impl Default for SovereigntyLevel {
        fn default() -> Self {
            SovereigntyLevel::Local
        }
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
        NodeDegraded {
            node_id: NodeId,
            reason: String,
        },
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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

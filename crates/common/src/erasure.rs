//! Erasure coding trait and types.
//!
//! Defines a pluggable erasure coding interface supporting Reed-Solomon,
//! ISA-L (hardware-accelerated), or LRC (locally repairable codes).
//!
//! ## Architecture
//!
//! Data is divided into `k` data shards. The encoder produces `m` parity
//! shards. Any `k` of the `k + m` shards are sufficient to reconstruct
//! the original data.
//!
//! Shard IDs are typed to prevent accidental misuse of raw indices.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Typed shard identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ShardId(pub u32);

/// Set of shard IDs.
pub type ShardIdSet = BTreeSet<ShardId>;

/// Map from shard ID to shard data.
pub type ShardIdMap = BTreeMap<ShardId, Vec<u8>>;

/// Erasure coding profile (stored per-pool or per-capsule policy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErasureProfile {
    /// Number of data shards (k).
    pub data_shards: u32,
    /// Number of parity shards (m).
    pub parity_shards: u32,
    /// Algorithm identifier, e.g. `"reed-solomon"`, `"isa-l"`, `"lrc"`.
    pub algorithm: String,
}

impl ErasureProfile {
    pub fn new(data_shards: u32, parity_shards: u32, algorithm: impl Into<String>) -> Self {
        Self {
            data_shards,
            parity_shards,
            algorithm: algorithm.into(),
        }
    }

    /// Total shard count: k + m.
    pub fn shard_count(&self) -> u32 {
        self.data_shards + self.parity_shards
    }

    /// Storage overhead ratio: (k + m) / k.
    pub fn overhead_ratio(&self) -> f64 {
        self.shard_count() as f64 / self.data_shards as f64
    }
}

impl Default for ErasureProfile {
    fn default() -> Self {
        Self::new(6, 2, "reed-solomon")
    }
}

/// Error type for erasure coding operations.
#[derive(Debug)]
pub enum ErasureError {
    InsufficientShards { available: usize, required: usize },
    InvalidShardSize { expected: usize, actual: usize },
    MissingShard(ShardId),
    EncodingFailed(String),
    DecodingFailed(String),
}

impl std::fmt::Display for ErasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientShards {
                available,
                required,
            } => {
                write!(f, "insufficient shards: have {available}, need {required}")
            }
            Self::InvalidShardSize { expected, actual } => {
                write!(f, "invalid shard size: expected {expected}, got {actual}")
            }
            Self::MissingShard(id) => write!(f, "shard {id:?} not found"),
            Self::EncodingFailed(msg) => write!(f, "encoding failed: {msg}"),
            Self::DecodingFailed(msg) => write!(f, "decoding failed: {msg}"),
        }
    }
}

impl std::error::Error for ErasureError {}

/// Pluggable erasure coding engine.
///
/// Implementations provide `encode` and `decode` for a specific algorithm.
pub trait ErasureCode: Send + Sync {
    /// Initialize from a profile. Called once at construction.
    fn init(&mut self, profile: &ErasureProfile) -> Result<(), ErasureError>;

    /// Number of data shards (k).
    fn data_shard_count(&self) -> u32;

    /// Number of parity/coding shards (m).
    fn parity_shard_count(&self) -> u32;

    /// Total shard count (k + m).
    fn shard_count(&self) -> u32 {
        self.data_shard_count() + self.parity_shard_count()
    }

    /// Compute the per-shard size given a stripe width (total data length).
    fn shard_size(&self, data_len: usize) -> usize {
        let k = self.data_shard_count() as usize;
        // Pad up to a multiple of k.
        data_len.div_ceil(k)
    }

    /// Encode `data` into `k + m` shards.
    ///
    /// Returns a map from `ShardId` to shard bytes. Shards `0..k` contain
    /// data; shards `k..k+m` contain parity.
    fn encode(&self, data: &[u8]) -> Result<ShardIdMap, ErasureError>;

    /// Decode original data from any `k` of the `k + m` shards.
    ///
    /// `available` must contain at least `k` shards. The implementation
    /// determines the minimal subset needed.
    fn decode(&self, available: &ShardIdMap, data_len: usize) -> Result<Vec<u8>, ErasureError>;

    /// Given a set of wanted shards and a set of available shards,
    /// compute the minimum set of shards needed for reconstruction.
    ///
    /// Compute the minimal reconstruction set — enables repair-bandwidth optimization.
    fn minimum_to_decode(
        &self,
        want: &ShardIdSet,
        available: &ShardIdSet,
    ) -> Result<ShardIdSet, ErasureError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_6_2() {
        let p = ErasureProfile::default();
        assert_eq!(p.data_shards, 6);
        assert_eq!(p.parity_shards, 2);
        assert_eq!(p.shard_count(), 8);
        assert!((p.overhead_ratio() - 1.333).abs() < 0.01);
    }

    #[test]
    fn shard_id_ordering() {
        let mut set = ShardIdSet::new();
        set.insert(ShardId(3));
        set.insert(ShardId(1));
        set.insert(ShardId(2));
        let ids: Vec<_> = set.iter().map(|s| s.0).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    // ── ErasureProfile construction ─────────────────────────────────

    #[test]
    fn profile_new_custom() {
        let p = ErasureProfile::new(4, 3, "isa-l");
        assert_eq!(p.data_shards, 4);
        assert_eq!(p.parity_shards, 3);
        assert_eq!(p.shard_count(), 7);
        assert_eq!(p.algorithm, "isa-l");
    }

    #[test]
    fn profile_overhead_ratio_typical_values() {
        // k=4, m=2 → 6/4 = 1.5
        let p = ErasureProfile::new(4, 2, "rs");
        assert!((p.overhead_ratio() - 1.5).abs() < 0.001);

        // k=10, m=4 → 14/10 = 1.4
        let p = ErasureProfile::new(10, 4, "rs");
        assert!((p.overhead_ratio() - 1.4).abs() < 0.001);
    }

    #[test]
    fn profile_zero_parity_overhead_is_one() {
        let p = ErasureProfile::new(6, 0, "none");
        assert_eq!(p.shard_count(), 6);
        assert!((p.overhead_ratio() - 1.0).abs() < 0.001);
    }

    #[test]
    fn profile_serde_roundtrip() {
        let p = ErasureProfile::new(8, 3, "reed-solomon");
        let json = serde_json::to_string(&p).unwrap();
        let restored: ErasureProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, restored);
    }

    #[test]
    fn profile_equality() {
        let a = ErasureProfile::new(6, 2, "reed-solomon");
        let b = ErasureProfile::default();
        assert_eq!(a, b);

        let c = ErasureProfile::new(6, 3, "reed-solomon");
        assert_ne!(a, c);
    }

    // ── ErasureError Display ────────────────────────────────────────

    #[test]
    fn error_insufficient_shards_display() {
        let e = ErasureError::InsufficientShards {
            available: 3,
            required: 6,
        };
        let msg = format!("{e}");
        assert!(msg.contains("3"));
        assert!(msg.contains("6"));
        assert!(msg.contains("insufficient"));
    }

    #[test]
    fn error_invalid_shard_size_display() {
        let e = ErasureError::InvalidShardSize {
            expected: 1024,
            actual: 512,
        };
        let msg = format!("{e}");
        assert!(msg.contains("1024"));
        assert!(msg.contains("512"));
    }

    #[test]
    fn error_missing_shard_display() {
        let e = ErasureError::MissingShard(ShardId(7));
        let msg = format!("{e}");
        assert!(msg.contains("7"));
    }

    #[test]
    fn error_encoding_failed_display() {
        let e = ErasureError::EncodingFailed("codec error".to_string());
        let msg = format!("{e}");
        assert!(msg.contains("codec error"));
    }

    #[test]
    fn error_decoding_failed_display() {
        let e = ErasureError::DecodingFailed("data corrupt".to_string());
        let msg = format!("{e}");
        assert!(msg.contains("data corrupt"));
    }

    #[test]
    fn error_implements_std_error() {
        let e = ErasureError::EncodingFailed("test".into());
        let _: &dyn std::error::Error = &e; // compile-time check
    }

    // ── ShardId ─────────────────────────────────────────────────────

    #[test]
    fn shard_id_equality_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ShardId(1));
        set.insert(ShardId(1)); // duplicate
        set.insert(ShardId(2));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn shard_id_serde_roundtrip() {
        let id = ShardId(42);
        let json = serde_json::to_string(&id).unwrap();
        let restored: ShardId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    // ── ShardIdMap ──────────────────────────────────────────────────

    #[test]
    fn shard_id_map_ordered_by_id() {
        let mut map = ShardIdMap::new();
        map.insert(ShardId(5), vec![5]);
        map.insert(ShardId(1), vec![1]);
        map.insert(ShardId(3), vec![3]);

        let keys: Vec<u32> = map.keys().map(|k| k.0).collect();
        assert_eq!(keys, vec![1, 3, 5], "BTreeMap should maintain order");
    }
}

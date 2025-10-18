use serde::{Deserialize, Serialize};

/// Compression algorithm selection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompressionPolicy {
    /// No compression
    None,
    /// LZ4 fast compression (level 1-16)
    LZ4 { level: i32 },
    /// Zstd balanced compression (level 1-22)
    Zstd { level: i32 },
}

impl Default for CompressionPolicy {
    fn default() -> Self {
        // Default to LZ4 level 1 for speed
        CompressionPolicy::LZ4 { level: 1 }
    }
}

/// Storage efficiency policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Compression algorithm and level
    pub compression: CompressionPolicy,
    
    /// Enable inline deduplication
    pub dedupe: bool,
    
    /// Background compaction interval in seconds (None = disabled)
    pub compact_interval_secs: Option<u64>,
    
    /// Erasure coding profile (future use)
    pub erasure_profile: Option<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            compression: CompressionPolicy::default(),
            dedupe: true,
            compact_interval_secs: Some(3600), // 1 hour
            erasure_profile: None,
        }
    }
}

impl Policy {
    /// Create a policy optimized for text/logs (high compression)
    pub fn text_optimized() -> Self {
        Self {
            compression: CompressionPolicy::Zstd { level: 3 },
            dedupe: true,
            compact_interval_secs: Some(1800),
            erasure_profile: None,
        }
    }

    /// Create a policy for pre-compressed data (skip compression)
    pub fn precompressed() -> Self {
        Self {
            compression: CompressionPolicy::None,
            dedupe: false,
            compact_interval_secs: Some(7200),
            erasure_profile: None,
        }
    }

    /// Create a policy for edge nodes (minimal overhead)
    pub fn edge_optimized() -> Self {
        Self {
            compression: CompressionPolicy::LZ4 { level: 1 },
            dedupe: false,
            compact_interval_secs: None, // Manual compaction
            erasure_profile: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = Policy::default();
        assert!(policy.dedupe);
        assert!(matches!(policy.compression, CompressionPolicy::LZ4 { level: 1 }));
    }

    #[test]
    fn test_policy_presets() {
        let text = Policy::text_optimized();
        assert!(matches!(text.compression, CompressionPolicy::Zstd { .. }));

        let precomp = Policy::precompressed();
        assert!(matches!(precomp.compression, CompressionPolicy::None));
        assert!(!precomp.dedupe);

        let edge = Policy::edge_optimized();
        assert!(edge.compact_interval_secs.is_none());
    }

    #[test]
    fn test_serialization() {
        let policy = Policy::default();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: Policy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy.dedupe, deserialized.dedupe);
    }
}
use std::collections::HashMap;

use anyhow::Result;
use common::{traits::Deduper, ContentHash, SegmentId};

pub use common::traits::DedupStats;

/// Compute BLAKE3 hash of data for deduplication.
pub fn hash_content(data: &[u8]) -> ContentHash {
    let hash = blake3::hash(data);
    ContentHash::from_bytes(hash.as_bytes())
}

/// Basic in-memory deduper backed by a hash map.
pub struct Blake3Deduper {
    index: HashMap<ContentHash, SegmentId>,
    stats: DedupStats,
}

impl Blake3Deduper {
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
            stats: DedupStats::new(),
        }
    }

    fn stats_mut(&mut self) -> &mut DedupStats {
        &mut self.stats
    }
}

impl Default for Blake3Deduper {
    fn default() -> Self {
        Self::new()
    }
}

impl Deduper for Blake3Deduper {
    fn hash_content(&self, data: &[u8]) -> ContentHash {
        hash_content(data)
    }

    fn check_dedup(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.index.get(hash).copied()
    }

    fn register_content(&mut self, hash: ContentHash, segment: SegmentId) -> Result<()> {
        self.index.insert(hash, segment);
        Ok(())
    }

    fn update_stats(&mut self, segment_len: u64, was_deduped: bool) {
        self.stats_mut().add_segment(segment_len, was_deduped);
    }

    fn stats(&self) -> DedupStats {
        self.stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content() {
        let data1 = b"Hello SPACE!";
        let data2 = b"Hello SPACE!";
        let data3 = b"Different data";

        let hash1 = hash_content(data1);
        let hash2 = hash_content(data2);
        let hash3 = hash_content(data3);

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_dedup_stats_tracking() {
        let mut deduper = Blake3Deduper::new();
        deduper.update_stats(4_000_000, false);
        deduper.update_stats(4_000_000, true);
        deduper.update_stats(4_000_000, true);

        let mut stats = deduper.stats();
        stats.compute_ratio();

        assert_eq!(stats.total_segments, 3);
        assert_eq!(stats.deduped_segments, 2);
        assert!(stats.dedup_ratio >= 1.0);
    }
}

use anyhow::Result;
use blake3;
use common::ContentHash;

/// Compute BLAKE3 hash of data
/// 
/// Uses BLAKE3 for speed and cryptographic strength.
/// This hash is used for content-addressed storage.
pub fn hash_content(data: &[u8]) -> ContentHash {
    let hash = blake3::hash(data);
    ContentHash::from_bytes(hash.as_bytes())
}

/// Deduplication statistics for reporting
#[derive(Debug, Clone)]
pub struct DedupStats {
    pub total_segments: usize,
    pub deduped_segments: usize,
    pub bytes_saved: u64,
    pub dedup_ratio: f32,
}

impl DedupStats {
    pub fn new() -> Self {
        Self {
            total_segments: 0,
            deduped_segments: 0,
            bytes_saved: 0,
            dedup_ratio: 1.0,
        }
    }

    pub fn add_segment(&mut self, size: u64, was_deduped: bool) {
        self.total_segments += 1;
        if was_deduped {
            self.deduped_segments += 1;
            self.bytes_saved += size;
        }
    }

    pub fn compute_ratio(&mut self) {
        if self.bytes_saved > 0 && self.total_segments > 0 {
            let total_bytes = self.total_segments as u64 * 4 * 1024 * 1024; // Assuming 4MB avg
            self.dedup_ratio = total_bytes as f32 / (total_bytes - self.bytes_saved) as f32;
        }
    }
}

impl Default for DedupStats {
    fn default() -> Self {
        Self::new()
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

        // Same content = same hash
        assert_eq!(hash1, hash2);
        
        // Different content = different hash
        assert_ne!(hash1, hash3);
        
        println!("✅ Content hashing works correctly");
    }

    #[test]
    fn test_dedup_stats() {
        let mut stats = DedupStats::new();
        
        stats.add_segment(4_000_000, false); // New segment
        stats.add_segment(4_000_000, true);  // Deduped
        stats.add_segment(4_000_000, true);  // Deduped
        
        assert_eq!(stats.total_segments, 3);
        assert_eq!(stats.deduped_segments, 2);
        assert_eq!(stats.bytes_saved, 8_000_000);
        
        stats.compute_ratio();
        assert!(stats.dedup_ratio > 1.0);
        
        println!("✅ Dedup stats tracking works");
    }

    #[test]
    fn test_blake3_consistency() {
        // Verify BLAKE3 produces consistent 32-byte hashes
        let data = b"Test data for consistency";
        let hash = hash_content(data);
        
        // BLAKE3 produces 32-byte hash, hex encoded = 64 chars
        assert_eq!(hash.as_str().len(), 64);
        
        // Hash again - should be identical
        let hash2 = hash_content(data);
        assert_eq!(hash, hash2);
        
        println!("✅ BLAKE3 hashing is consistent");
    }
}
use std::collections::HashMap;

use anyhow::Result;
use common::{traits::Deduper, ContentHash, SegmentId};

pub use common::traits::DedupStats;

/// Compute BLAKE3 hash of data for deduplication, with no algorithm context.
///
/// Prefer [`hash_content_with_algo`] on pipeline write paths — using a bare
/// content hash there allows two segments that happen to have the same stored
/// bytes but different decompression treatments (e.g. an LZ4 frame stored raw
/// under `CompressionPolicy::None` vs the original plaintext compressed under
/// `CompressionPolicy::LZ4`) to collide in the dedup index, which produces
/// silently-wrong reads after dedup hits.
///
/// This entry point remains for callers that genuinely have no compression
/// context (raw blob hashing, integrity tests, etc.).
pub fn hash_content(data: &[u8]) -> ContentHash {
    let hash = blake3::hash(data);
    ContentHash::from_bytes(hash.as_bytes())
}

/// Compute a domain-separated BLAKE3 hash for deduplication.
///
/// The dedup index must guarantee `key(a) == key(b) ⇒ read(a) == read(b)`.
/// Hashing only the stored bytes breaks that invariant when two writes land
/// the same bytes through different compression treatments. Mixing the
/// compression algorithm name into the hash domain prevents cross-policy
/// reuse: `(raw_lz4_frame, "identity")` and `(plaintext_compressed, "lz4:1")`
/// produce different keys even when the byte sequences match.
///
/// `algo` should be the value of `CompressionResult::algorithm` from the
/// segment that produced `data` (e.g. `"identity"`, `"lz4:1"`, `"zstd:3"`).
pub fn hash_content_with_algo(data: &[u8], algo: &str) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    // Versioned domain prefix so we can rotate the scheme later without
    // colliding with older keys. The trailing NUL separates the algo string
    // from `data` so e.g. `"lz4"` + `":1\0..."` cannot alias `"lz4:1"` + `"\0..."`.
    hasher.update(b"space.dedup.v1\0algo:");
    hasher.update(algo.as_bytes());
    hasher.update(b"\0");
    hasher.update(data);
    ContentHash::from_bytes(hasher.finalize().as_bytes())
}

/// Outcome of [`verify_content_hash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Hash matches under the algo-domain-separated scheme (post-fix).
    Matched,
    /// Hash matches only under the bare-BLAKE3 scheme.
    ///
    /// Segments written before the cross-policy dedup fix recorded a bare
    /// `hash_content(data)`; their `compression_algo` is still non-empty, so
    /// we accept the legacy form during the migration window. Operators can
    /// gauge progress via `ScrubReport::legacy_hash_hits`.
    LegacyMatched,
    /// Hash does not match under either scheme — genuine corruption or tampering.
    Mismatched,
}

/// Verify a stored `content_hash` against bytes plus the recorded compression
/// algorithm, accepting both the current (algo-domain-separated) scheme and the
/// legacy bare-BLAKE3 scheme.
///
/// Callers (notably the scrub executor) treat `Matched` and `LegacyMatched`
/// as integrity-clean and `Mismatched` as bitrot. Tracking `LegacyMatched`
/// separately lets operators measure how much pre-fix data is still in flight
/// before the bare-hash fallback can be removed.
pub fn verify_content_hash(expected: &ContentHash, data: &[u8], algo: &str) -> VerifyOutcome {
    if !algo.is_empty() {
        let with_algo = hash_content_with_algo(data, algo);
        if with_algo.as_str() == expected.as_str() {
            return VerifyOutcome::Matched;
        }
    }
    let bare = hash_content(data);
    if bare.as_str() == expected.as_str() {
        VerifyOutcome::LegacyMatched
    } else {
        VerifyOutcome::Mismatched
    }
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

    fn hash_content_with_algo(&self, data: &[u8], algo: &str) -> ContentHash {
        hash_content_with_algo(data, algo)
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
    fn verify_content_hash_matches_algo_scheme() {
        let data = b"payload bytes";
        let algo = "lz4:1";
        let expected = hash_content_with_algo(data, algo);
        assert_eq!(
            verify_content_hash(&expected, data, algo),
            VerifyOutcome::Matched
        );
    }

    #[test]
    fn verify_content_hash_falls_back_to_bare_for_legacy_data() {
        let data = b"legacy payload";
        // Pre-fix layout: hash is bare BLAKE3 but algo metadata is still recorded.
        let legacy = hash_content(data);
        assert_eq!(
            verify_content_hash(&legacy, data, "identity"),
            VerifyOutcome::LegacyMatched
        );
    }

    #[test]
    fn verify_content_hash_reports_mismatch_on_corruption() {
        let original = b"original bytes!";
        let corrupted = b"corrupted bytes"; // same length
        let expected = hash_content_with_algo(original, "zstd:3");
        assert_eq!(
            verify_content_hash(&expected, corrupted, "zstd:3"),
            VerifyOutcome::Mismatched
        );
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

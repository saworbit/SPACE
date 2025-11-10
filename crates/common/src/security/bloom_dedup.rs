use std::sync::{
    atomic::{AtomicU8, AtomicUsize, Ordering},
    Arc,
};

use blake3::Hasher;

use crate::ContentHash;

/// Provides a trait for deduplication accelerators (e.g., Bloom filters).
pub trait DedupOptimizer: Send + Sync {
    fn might_contain(&self, hash: &ContentHash) -> bool;
    fn record_insertion(&self, hash: &ContentHash);
    fn record_removal(&self, hash: &ContentHash);
}

/// High-level wrapper around the counting Bloom filter.
#[derive(Clone)]
pub struct BloomFilterWrapper {
    inner: Arc<CountingBloomFilter>,
}

impl BloomFilterWrapper {
    pub fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        Self {
            inner: Arc::new(CountingBloomFilter::new(
                expected_items,
                false_positive_rate,
            )),
        }
    }

    /// Populate an instance from an iterator of existing hashes.
    pub fn with_existing<I>(expected_items: usize, false_positive_rate: f64, hashes: I) -> Self
    where
        I: IntoIterator<Item = ContentHash>,
    {
        let filter = CountingBloomFilter::new(expected_items, false_positive_rate);
        let wrapper = Self {
            inner: Arc::new(filter),
        };
        for hash in hashes {
            wrapper.record_insertion(&hash);
        }
        wrapper
    }

    pub fn stats(&self) -> BloomStats {
        BloomStats {
            buckets: self.inner.bucket_count,
            num_hashes: self.inner.num_hashes,
            insertions: self.inner.items.load(Ordering::Relaxed),
        }
    }
}

impl DedupOptimizer for BloomFilterWrapper {
    fn might_contain(&self, hash: &ContentHash) -> bool {
        self.inner
            .might_contain(hash.as_str().as_bytes())
            .unwrap_or(true)
    }

    fn record_insertion(&self, hash: &ContentHash) {
        self.inner
            .insert(hash.as_str().as_bytes())
            .expect("bloom insert");
    }

    fn record_removal(&self, hash: &ContentHash) {
        self.inner
            .remove(hash.as_str().as_bytes())
            .expect("bloom remove");
    }
}

#[derive(Debug, Clone)]
pub struct BloomStats {
    pub buckets: usize,
    pub num_hashes: u32,
    pub insertions: usize,
}

struct CountingBloomFilter {
    counters: Vec<AtomicU8>,
    bucket_count: usize,
    num_hashes: u32,
    items: AtomicUsize,
}

impl CountingBloomFilter {
    fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        let expected = expected_items.max(1) as f64;
        let fp = false_positive_rate
            .clamp(0.000_000_1, 0.25)
            .max(f64::MIN_POSITIVE);
        let ln2_sq = std::f64::consts::LN_2.powi(2);
        let bucket_count = ((-expected * fp.ln()) / ln2_sq).ceil() as usize;
        let bucket_count = bucket_count.max(1024);
        let num_hashes = ((bucket_count as f64 / expected) * std::f64::consts::LN_2)
            .ceil()
            .max(1.0) as u32;

        let counters = (0..bucket_count)
            .map(|_| AtomicU8::new(0))
            .collect::<Vec<_>>();

        Self {
            counters,
            bucket_count,
            num_hashes,
            items: AtomicUsize::new(0),
        }
    }

    fn insert(&self, data: &[u8]) -> anyhow::Result<()> {
        for idx in self.indexes(data) {
            let cell = self
                .counters
                .get(idx)
                .ok_or_else(|| anyhow::anyhow!("index out of range"))?;
            cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                if val == u8::MAX {
                    Some(u8::MAX)
                } else {
                    Some(val + 1)
                }
            })
            .ok();
        }
        self.items.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn remove(&self, data: &[u8]) -> anyhow::Result<()> {
        for idx in self.indexes(data) {
            let cell = self
                .counters
                .get(idx)
                .ok_or_else(|| anyhow::anyhow!("index out of range"))?;
            cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                if val == 0 {
                    Some(0)
                } else {
                    Some(val - 1)
                }
            })
            .ok();
        }
        self.items
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                Some(val.saturating_sub(1))
            })
            .ok();
        Ok(())
    }

    fn might_contain(&self, data: &[u8]) -> anyhow::Result<bool> {
        for idx in self.indexes(data) {
            if let Some(cell) = self.counters.get(idx) {
                if cell.load(Ordering::Relaxed) == 0 {
                    return Ok(false);
                }
            } else {
                return Ok(true);
            }
        }
        Ok(true)
    }

    fn indexes<'a>(&'a self, data: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
        let (h1, h2) = hash_pair(data);
        (0..self.num_hashes).map(move |i| {
            let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
            (combined % self.bucket_count as u64) as usize
        })
    }
}

fn hash_pair(data: &[u8]) -> (u64, u64) {
    let mut out = [0u8; 16];
    let mut hasher = Hasher::new();
    hasher.update(data);
    let mut reader = hasher.finalize_xof();
    reader.fill(&mut out);

    let mut first = [0u8; 8];
    let mut second = [0u8; 8];
    first.copy_from_slice(&out[..8]);
    second.copy_from_slice(&out[8..]);

    (u64::from_le_bytes(first), u64::from_le_bytes(second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_insert_lookup() {
        let filter = BloomFilterWrapper::new(1_000, 0.001);
        let hash = ContentHash("deadbeef".into());
        assert!(!filter.might_contain(&hash));
        filter.record_insertion(&hash);
        assert!(filter.might_contain(&hash));
        filter.record_removal(&hash);
        assert!(!filter.might_contain(&hash));
    }
}

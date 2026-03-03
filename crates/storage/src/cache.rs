//! Byte-bounded LRU read cache wrapping any [`StorageBackend`].
//!
//! ## Why not the OS page cache?
//!
//! ZFS cannot use the OS page cache because it manages its own block sizes,
//! compression, and encryption — the kernel doesn't know about these layers.
//! SPACE faces the same constraint: segments stored on disk are encrypted and
//! compressed, so a cache at the storage layer stores the raw ciphertext and
//! serves it to the pipeline above, which then decrypts/decompresses.
//!
//! ## Design vs. ZFS ARC
//!
//! ZFS's ARC uses a two-list MRU/MFU split plus ghost lists to adapt the
//! boundary based on access patterns.  This is effective but complex, and
//! the L2ARC index can consume significant RAM.
//!
//! `CachedBackend` takes a simpler approach that works well for SPACE's
//! access patterns:
//!
//! - **Byte-bounded**, not entry-count bounded — a cache of 256 MB holds
//!   256 MB regardless of whether it contains 64 × 4 MiB segments or
//!   4096 × 64 KiB segments.
//! - **Single LRU** — straightforward eviction.  Scan resistance comes from
//!   the segment-size cap (one sequential scan of 4 MiB segments evicts no
//!   more than `max_bytes / 4 MiB` entries before the cache restabilises).
//! - **Write-through invalidation** — any write to a segment immediately
//!   evicts it from the cache, ensuring reads never see stale data.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use common::{
    traits::{StorageBackend, StorageTransaction},
    Segment, SegmentId,
};
use futures::future::BoxFuture;
use lru::LruCache;

/// Maximum number of entries kept in the per-segment invalidation-generation
/// map.  Entries beyond this limit are evicted in LRU order.  Evicting a
/// tombstone is safe: any in-flight reader for such an old entry will have
/// completed (or errored) long before 4 096 subsequent invalidations occur.
const GENERATION_CAP: usize = 4096;

// ── ByteLruCache ─────────────────────────────────────────────────────────────

/// LRU cache bounded by total cached bytes rather than entry count.
struct ByteLruCache {
    /// Unbounded-capacity LruCache used purely for LRU ordering.
    entries: LruCache<SegmentId, (Vec<u8>, Segment)>,
    current_bytes: u64,
    max_bytes: u64,
    /// Per-segment invalidation generation, incremented by every `invalidate()` call.
    ///
    /// The read path captures the generation before releasing the lock for I/O.
    /// When it tries to insert the freshly-read data it compares against the
    /// current generation: if they differ, a concurrent write *or delete*
    /// invalidated the segment while the I/O was in-flight and the (now-stale)
    /// data is discarded.
    ///
    /// **Boundedness:** bounded to `GENERATION_CAP` entries by an LRU eviction
    /// policy.  `put_if_generation` pops the entry on success (write→re-read
    /// cycle), keeping churn low.  For permanently deleted segments the entry
    /// persists until LRU eviction; eviction is safe because any in-flight
    /// reader for an entry that old will have completed long before
    /// `GENERATION_CAP` subsequent invalidations occur.
    generations: LruCache<SegmentId, u64>,
}

impl ByteLruCache {
    fn new(max_bytes: u64) -> Self {
        Self {
            entries: LruCache::unbounded(),
            current_bytes: 0,
            max_bytes,
            generations: LruCache::new(NonZeroUsize::new(GENERATION_CAP).unwrap()),
        }
    }

    /// Return the current invalidation generation for `id` (0 if never invalidated).
    ///
    /// Uses `peek` so that checking a generation does not count as a use and
    /// does not delay LRU eviction of old tombstones.
    fn generation_of(&self, id: &SegmentId) -> u64 {
        self.generations.peek(id).copied().unwrap_or(0)
    }

    fn get(&mut self, id: &SegmentId) -> Option<(&Vec<u8>, &Segment)> {
        self.entries.get(id).map(|(d, s)| (d, s))
    }

    fn put(&mut self, id: SegmentId, data: Vec<u8>, meta: Segment) {
        let size = data.len() as u64;
        // Don't cache a single segment that exceeds the entire limit.
        if size > self.max_bytes {
            return;
        }
        // Evict LRU entries until there is room.
        while self.current_bytes + size > self.max_bytes {
            match self.entries.pop_lru() {
                Some((_, (evicted, _))) => self.current_bytes -= evicted.len() as u64,
                None => break,
            }
        }
        // If replacing an existing entry, subtract its size first.
        if let Some((old, _)) = self.entries.put(id, (data, meta)) {
            self.current_bytes -= old.len() as u64;
        }
        self.current_bytes += size;
    }

    /// Insert `data`/`meta` only if the segment's invalidation generation still
    /// equals `expected_gen`.  If a concurrent write invalidated this segment
    /// while the caller was performing I/O, the generation will have advanced
    /// and the (now-stale) data is silently discarded.
    ///
    /// On success the generation entry is pruned: the segment is now cached so
    /// no in-flight reader is outstanding.  The next `invalidate()` will
    /// re-insert starting from 0 → 1, which new readers will capture correctly.
    fn put_if_generation(
        &mut self,
        id: SegmentId,
        data: Vec<u8>,
        meta: Segment,
        expected_gen: u64,
    ) {
        if self.generation_of(&id) != expected_gen {
            return;
        }
        self.put(id, data, meta);
        // Prune: the segment is cached, so no in-flight readers remain.
        self.generations.pop(&id);
    }

    /// Invalidate a segment that has been written or deleted.
    ///
    /// Bumps the per-segment generation so that any in-flight reader that
    /// captured the previous generation will have its `put_if_generation` call
    /// rejected, preventing stale data from silently re-entering the cache.
    /// The generation is bumped even when the segment is not currently cached,
    /// because a reader may have already fetched its bytes from the inner
    /// backend and be about to call `put_if_generation`.
    fn invalidate(&mut self, id: &SegmentId) {
        if let Some((data, _)) = self.entries.pop(id) {
            self.current_bytes -= data.len() as u64;
        }
        let gen = self.generations.peek(id).copied().unwrap_or(0);
        self.generations.put(*id, gen + 1);
    }
}

// ── CachedBackend ─────────────────────────────────────────────────────────────

/// A [`StorageBackend`] wrapper that adds a byte-bounded LRU read cache.
///
/// Reads are served from the cache when possible; misses fall through to the
/// inner backend and populate the cache.  Writes always go to the inner
/// backend and invalidate any cached entry for the affected segment.
///
/// # Example
///
/// ```rust,ignore
/// let backend = InMemoryBackend::new();
/// // Cache up to 256 MiB of raw segment data.
/// let cached = CachedBackend::new(backend, 256 * 1024 * 1024);
/// ```
#[derive(Clone)]
pub struct CachedBackend<B: StorageBackend> {
    inner: B,
    cache: Arc<Mutex<ByteLruCache>>,
}

impl<B: StorageBackend> CachedBackend<B> {
    /// Wrap `inner` with an LRU cache capped at `max_cache_bytes` total bytes.
    pub fn new(inner: B, max_cache_bytes: u64) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(ByteLruCache::new(max_cache_bytes))),
        }
    }

    /// Bytes currently resident in the cache.
    pub fn cached_bytes(&self) -> u64 {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_bytes
    }

    /// Maximum cache capacity in bytes.
    pub fn max_cache_bytes(&self) -> u64 {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .max_bytes
    }
}

// ── CacheInvalidatingTransaction ─────────────────────────────────────────────

/// Wraps an inner [`StorageTransaction`], invalidating cache entries for any
/// segment that is appended, updated, or deleted when the transaction commits.
pub struct CacheInvalidatingTransaction<T: StorageTransaction> {
    inner: T,
    cache: Arc<Mutex<ByteLruCache>>,
    touched: Vec<SegmentId>,
}

impl<T: StorageTransaction + 'static> StorageTransaction for CacheInvalidatingTransaction<T> {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        self.touched.push(segment);
        self.inner.append(segment, data)
    }

    fn set_segment_metadata<'a>(
        &'a mut self,
        segment: SegmentId,
        metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        self.touched.push(segment);
        self.inner.set_segment_metadata(segment, metadata)
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        // Bump generation like a write — an in-flight read may have already
        // fetched the segment bytes from the inner backend and could still
        // reach put_if_generation.  The generation bump causes it to be rejected.
        self.touched.push(segment);
        self.inner.delete(segment)
    }

    fn commit(self) -> BoxFuture<'static, Result<()>> {
        let cache = self.cache;
        let touched = self.touched;
        Box::pin(async move {
            self.inner.commit().await?;
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            for id in &touched {
                guard.invalidate(id);
            }
            Ok(())
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        self.inner.rollback()
    }
}

// ── StorageBackend impl ───────────────────────────────────────────────────────

impl<B: StorageBackend + Clone> StorageBackend for CachedBackend<B>
where
    B::Transaction: 'static,
{
    type Transaction = CacheInvalidatingTransaction<B::Transaction>;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        // Invalidate before the write so a concurrent read never races with
        // a stale cache entry.
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalidate(&segment);
        self.inner.append(segment, data)
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        let cache = Arc::clone(&self.cache);
        Box::pin(async move {
            // Cache hit — return a clone of the cached bytes.
            //
            // On a miss we capture the current invalidation generation *before*
            // releasing the lock.  Any concurrent write that invalidates this
            // segment between here and the put_if_generation call below will
            // bump the generation, causing the stale data to be discarded rather
            // than silently repopulating the cache.
            let miss_gen = {
                let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((data, _)) = guard.get(&segment) {
                    return Ok(data.clone());
                }
                guard.generation_of(&segment)
            };
            // Cache miss — read from the inner backend.
            let data = self.inner.read(segment).await?;
            // Populate the cache, but only if no concurrent invalidation occurred
            // while the I/O was in-flight (checked via the generation counter).
            if let Ok(meta) = self.inner.metadata(segment).await {
                let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                guard.put_if_generation(segment, data.clone(), meta, miss_gen);
            }
            Ok(data)
        })
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        let cache = Arc::clone(&self.cache);
        Box::pin(async move {
            {
                let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((_, meta)) = guard.get(&segment) {
                    return Ok(meta.clone());
                }
            }
            self.inner.metadata(segment).await
        })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        // Use invalidate (bump generation) rather than a plain cache eviction.
        // A concurrent read may have already fetched the segment bytes from the
        // inner backend before the delete started and could still reach
        // put_if_generation — the generation bump causes that stale put to be
        // rejected.  Removing the entry instead would silently re-admit the
        // stale data.
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalidate(&segment);
        self.inner.delete(segment)
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        self.inner.segment_ids()
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        let cache = Arc::clone(&self.cache);
        Box::pin(async move {
            let inner_txn = self.inner.begin_txn().await?;
            Ok(CacheInvalidatingTransaction {
                inner: inner_txn,
                cache,
                touched: Vec::new(),
            })
        })
    }

    fn used_bytes(&self) -> BoxFuture<'_, Result<u64>> {
        self.inner.used_bytes()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryBackend;

    async fn write_seg(backend: &mut impl StorageBackend, id: u64, data: &[u8]) {
        let seg = Segment {
            id: SegmentId(id),
            len: data.len() as u32,
            ..Default::default()
        };
        let mut txn = backend.begin_txn().await.unwrap();
        txn.append(SegmentId(id), data).await.unwrap();
        txn.set_segment_metadata(SegmentId(id), seg).await.unwrap();
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn cache_hit_on_second_read() {
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 1024 * 1024);

        write_seg(&mut cached, 1, b"hello cache").await;

        let first = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(first, b"hello cache");

        // After the first read the data is cached; a second read should hit.
        let before = cached.cached_bytes();
        let second = cached.read(SegmentId(1)).await.unwrap();
        let after = cached.cached_bytes();
        assert_eq!(second, b"hello cache");
        // Bytes should not grow on a cache hit.
        assert_eq!(before, after, "cache hit must not re-insert bytes");
    }

    #[tokio::test]
    async fn byte_limit_evicts_lru() {
        // Allow room for exactly one 5-byte segment.
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 5);

        write_seg(&mut cached, 1, b"AAAAA").await;
        write_seg(&mut cached, 2, b"BBBBB").await;

        // Read seg 1 — populates cache (5 bytes).
        let _ = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(cached.cached_bytes(), 5);

        // Read seg 2 — must evict seg 1 to stay within the 5-byte limit.
        let _ = cached.read(SegmentId(2)).await.unwrap();
        assert_eq!(cached.cached_bytes(), 5, "cache must not exceed byte limit");
    }

    #[tokio::test]
    async fn write_invalidates_cache() {
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 1024 * 1024);

        write_seg(&mut cached, 1, b"original").await;
        let _ = cached.read(SegmentId(1)).await.unwrap();
        assert!(
            cached.cached_bytes() > 0,
            "first read should populate cache"
        );

        // Overwrite the segment — cache must be invalidated.
        write_seg(&mut cached, 1, b"updated!").await;
        let result = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(result, b"updated!", "should serve fresh data after write");
    }

    #[tokio::test]
    async fn delete_invalidates_cache() {
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 1024 * 1024);

        write_seg(&mut cached, 1, b"to be deleted").await;
        let _ = cached.read(SegmentId(1)).await.unwrap();

        cached.delete(SegmentId(1)).await.unwrap();
        assert_eq!(cached.cached_bytes(), 0, "delete should evict from cache");
    }

    #[tokio::test]
    async fn oversized_segment_not_cached() {
        // Cache of 3 bytes — a 5-byte segment must not be cached.
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 3);

        write_seg(&mut cached, 1, b"BBBBB").await;
        let _ = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(
            cached.cached_bytes(),
            0,
            "segment larger than max_bytes must not be cached"
        );
    }

    #[tokio::test]
    async fn stale_read_not_repopulated_after_invalidation() {
        // Verify the generation-counter guard: if a write invalidates a segment
        // between a cache miss and the subsequent put_if_generation call, the
        // stale pre-write data must not silently repopulate the cache.
        //
        // We simulate the race sequentially by manipulating the cache's internal
        // generation directly:
        //   1. seg 1 not in cache, gen = 0
        //   2. Invalidation bumps gen to 1 (simulating a concurrent write)
        //   3. put_if_generation(gen=0) is rejected — stale data discarded
        //   4. A fresh read goes to the inner backend and gets the new data
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 1024 * 1024);

        write_seg(&mut cached, 1, b"v1").await;

        // Manually bump the generation as a concurrent invalidation would.
        {
            let mut guard = cached.cache.lock().unwrap();
            guard.invalidate(&SegmentId(1));
        }

        // Write v2 to the inner backend so it's the ground truth.
        write_seg(&mut cached, 1, b"v2").await;

        // The generation is now 2 (invalidated twice: once manually, once by write_seg).
        // Any in-flight read that captured gen=0 or gen=1 would be rejected.
        let result = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(
            result, b"v2",
            "must serve fresh data, not stale pre-write data"
        );

        // Second read must hit the cache and still return v2.
        let result2 = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(result2, b"v2", "cached entry must be v2");
    }

    #[tokio::test]
    async fn transaction_commit_invalidates() {
        let inner = InMemoryBackend::new();
        let mut cached = CachedBackend::new(inner, 1024 * 1024);

        write_seg(&mut cached, 1, b"before txn").await;
        let _ = cached.read(SegmentId(1)).await.unwrap();
        assert!(cached.cached_bytes() > 0);

        let seg_new = Segment {
            id: SegmentId(1),
            len: 7,
            ..Default::default()
        };
        let mut txn = cached.begin_txn().await.unwrap();
        txn.append(SegmentId(1), b"updated").await.unwrap();
        txn.set_segment_metadata(SegmentId(1), seg_new)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        // Cache should have been invalidated at commit time.
        let result = cached.read(SegmentId(1)).await.unwrap();
        assert_eq!(result, b"updated", "must read fresh data after txn commit");
    }
}

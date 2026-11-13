use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use common::{
    traits::{StorageBackend, StorageTransaction},
    Segment, SegmentId,
};
use futures::future::{self, BoxFuture};
use nvram_sim::{NvramLog, NvramTransaction, RecountReport};

mod cache;
mod fs_root;
pub use cache::{CacheInvalidatingTransaction, CachedBackend};
use fs_root::{on_root, FsRoot};

#[cfg(all(target_os = "linux", feature = "uring"))]
mod uring;

#[cfg(all(target_os = "linux", feature = "uring"))]
pub use uring::UringBackend;

#[cfg(not(all(target_os = "linux", feature = "uring")))]
pub type UringBackend = TokioFsBackend;

#[derive(Default)]
struct Inner {
    segments: HashMap<SegmentId, Vec<u8>>,
    metadata: HashMap<SegmentId, Segment>,
}

/// In-memory storage backend used for testing and scaffolding.
#[derive(Clone, Default)]
pub struct InMemoryBackend {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct InMemoryTransaction {
    inner: Arc<Mutex<Inner>>,
    staged_segments: HashMap<SegmentId, Vec<u8>>,
    staged_metadata: HashMap<SegmentId, Segment>,
    deleted: Vec<SegmentId>,
}

impl InMemoryTransaction {
    fn new(inner: Arc<Mutex<Inner>>) -> Self {
        Self {
            inner,
            staged_segments: HashMap::new(),
            staged_metadata: HashMap::new(),
            deleted: Vec::new(),
        }
    }
}

impl StorageTransaction for InMemoryTransaction {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        self.staged_segments.insert(segment, data.to_vec());
        Box::pin(async { Ok(()) })
    }

    fn set_segment_metadata<'a>(
        &'a mut self,
        segment: SegmentId,
        metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        self.staged_metadata.insert(segment, metadata);
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        self.deleted.push(segment);
        Box::pin(async { Ok(()) })
    }

    fn commit(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for (segment, data) in self.staged_segments {
                guard.segments.insert(segment, data);
            }
            for (segment, metadata) in self.staged_metadata {
                guard.metadata.insert(segment, metadata);
            }
            for segment in self.deleted {
                guard.segments.remove(&segment);
                guard.metadata.remove(&segment);
            }
            Ok(())
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl StorageBackend for InMemoryBackend {
    type Transaction = InMemoryTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let inner = Arc::clone(&self.inner);
        let payload = data.to_vec();
        Box::pin(async move {
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.segments.insert(segment, payload);
            Ok(())
        })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .segments
                .get(&segment)
                .cloned()
                .ok_or_else(|| anyhow!("segment {:?} not found", segment))
        })
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .metadata
                .get(&segment)
                .cloned()
                .ok_or_else(|| anyhow!("segment {:?} metadata not found", segment))
        })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.segments.remove(&segment);
            guard.metadata.remove(&segment);
            Ok(())
        })
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            Ok(guard.metadata.keys().copied().collect())
        })
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(future::ready(Ok(InMemoryTransaction::new(inner))))
    }

    fn used_bytes(&self) -> futures::future::BoxFuture<'_, Result<u64>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            Ok(guard.segments.values().map(|b| b.len() as u64).sum())
        })
    }
}

/// NVRAM-backed storage implementation that wraps the legacy simulator.
#[derive(Clone)]
pub struct NvramBackend {
    log: NvramLog,
}

impl NvramBackend {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let log = NvramLog::open(path)?;
        Ok(Self { log })
    }

    pub fn from_log(log: NvramLog) -> Self {
        Self { log }
    }

    /// The data log this backend stores segments in.
    pub fn log(&self) -> &NvramLog {
        &self.log
    }
}

pub struct NvramStorageTransaction {
    inner: NvramTransaction,
    deleted: Vec<SegmentId>,
}

impl NvramStorageTransaction {
    fn new(inner: NvramTransaction) -> Self {
        Self {
            inner,
            deleted: Vec::new(),
        }
    }
}

impl StorageTransaction for NvramStorageTransaction {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let data_vec = data.to_vec();
        let inner = &mut self.inner;
        Box::pin(async move {
            inner.append_segment(segment, &data_vec)?;
            Ok(())
        })
    }

    fn set_segment_metadata<'a>(
        &'a mut self,
        segment: SegmentId,
        metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        let inner = &mut self.inner;
        Box::pin(async move {
            inner.set_segment_metadata(segment, metadata)?;
            Ok(())
        })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        self.deleted.push(segment);
        Box::pin(async { Ok(()) })
    }

    fn commit(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let mut txn = self.inner;
            let deletes = txn.log_handle();
            txn.commit()?;
            for seg in self.deleted {
                deletes.remove_segment(seg)?;
            }
            Ok(())
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let mut txn = self.inner;
            txn.rollback()?;
            Ok(())
        })
    }
}

impl StorageBackend for NvramBackend {
    type Transaction = NvramStorageTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let log = self.log.clone();
        let payload = data.to_vec();
        Box::pin(async move {
            log.append(segment, &payload)?;
            Ok(())
        })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        let log = self.log.clone();
        Box::pin(async move { log.read(segment) })
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        let log = self.log.clone();
        Box::pin(async move { log.get_segment_metadata(segment) })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        let log = self.log.clone();
        Box::pin(async move {
            log.remove_segment(segment)?;
            Ok(())
        })
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        let log = self.log.clone();
        Box::pin(async move { Ok(log.list_segment_ids()) })
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        let log = self.log.clone();
        Box::pin(async move {
            let txn = log.begin_transaction()?;
            Ok(NvramStorageTransaction::new(txn))
        })
    }

    fn used_bytes(&self) -> futures::future::BoxFuture<'_, Result<u64>> {
        let log = self.log.clone();
        Box::pin(async move { Ok(log.used_bytes()) })
    }

    /// Each record's frame checksums its stored bytes.
    fn verify_stored(
        &self,
        segment: SegmentId,
    ) -> futures::future::BoxFuture<'_, Result<Option<bool>>> {
        let log = self.log.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || log.verify_record(segment).map(Some)).await?
        })
    }
}

impl NvramBackend {
    /// Compact the underlying NVRAM log, reclaiming bytes from GC'd segments.
    ///
    /// Delegates to [`nvram_sim::NvramLog::compact`]. Returns bytes reclaimed.
    pub fn compact(&self) -> Result<u64> {
        self.log.compact()
    }
}

/// Filesystem-backed storage backend: a storage root in on-disk format 1
/// (`docs/on-disk-format.md` §10).
///
/// Layout:
/// - `root/store`: a `FILE` record holding the store id
/// - `root/segments/<id>.seg`: one framed `SEGMENT` record per segment, its
///   descriptor and stored bytes
#[derive(Clone)]
pub struct TokioFsBackend {
    root: Arc<FsRoot>,
}

impl TokioFsBackend {
    /// Open the storage root at `root`, creating it if it's new. A root in
    /// the layout from before on-disk format 1 is refused.
    pub async fn open<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let root = tokio::task::spawn_blocking(move || FsRoot::open(&root)).await??;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    /// The store id in `root/store`. The capsule journal is paired with it.
    pub fn store_id(&self) -> uuid::Uuid {
        self.root.store_id()
    }

    /// One past the highest segment id with a file.
    pub fn segment_id_floor(&self) -> Result<u64> {
        self.root.segment_id_floor()
    }

    /// Set every segment's ref count from `counts`, as
    /// [`NvramLog::recount_references`] does. Ref counts aren't stored, so
    /// this must run before the root serves writes or deletes.
    pub fn recount_references(&self, counts: &HashMap<SegmentId, u32>) -> Result<RecountReport> {
        self.root.recount_references(counts)
    }
}

pub struct TokioFsTransaction {
    root: Arc<FsRoot>,
    staged_segments: HashMap<SegmentId, Vec<u8>>,
    staged_metadata: HashMap<SegmentId, Segment>,
    deleted: Vec<SegmentId>,
}

impl TokioFsTransaction {
    fn new(root: Arc<FsRoot>) -> Self {
        Self {
            root,
            staged_segments: HashMap::new(),
            staged_metadata: HashMap::new(),
            deleted: Vec::new(),
        }
    }
}

impl StorageTransaction for TokioFsTransaction {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        self.staged_segments.insert(segment, data.to_vec());
        Box::pin(async { Ok(()) })
    }

    fn set_segment_metadata<'a>(
        &'a mut self,
        segment: SegmentId,
        metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        self.staged_metadata.insert(segment, metadata);
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        self.deleted.push(segment);
        Box::pin(async { Ok(()) })
    }

    /// Each segment's file is written whole, with its descriptor, and
    /// fsynced before this returns.
    fn commit(self) -> BoxFuture<'static, Result<()>> {
        let Self {
            root,
            staged_segments,
            staged_metadata,
            deleted,
        } = self;
        on_root(&root, move |root| {
            root.commit(staged_segments, staged_metadata, deleted)
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl StorageBackend for TokioFsBackend {
    type Transaction = TokioFsTransaction;

    /// Write a segment with no descriptor beyond its bytes: stored as is,
    /// with no content hash.
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let payload = data.to_vec();
        on_root(&self.root, move |root| {
            root.write_segment(segment, &payload, None)
        })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        on_root(&self.root, move |root| root.read(segment))
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        on_root(&self.root, move |root| root.metadata(segment))
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        on_root(&self.root, move |root| root.delete(segment))
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        on_root(&self.root, |root| root.segment_ids())
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        Box::pin(future::ready(Ok(TokioFsTransaction::new(Arc::clone(
            &self.root,
        )))))
    }

    /// Each segment file's record checksums its descriptor and stored bytes.
    fn verify_stored(&self, segment: SegmentId) -> BoxFuture<'_, Result<Option<bool>>> {
        on_root(&self.root, move |root| root.verify(segment).map(Some))
    }
}

/// Runtime-selected filesystem backend.
///
/// On Linux with `storage/uring`, this will attempt to use `UringBackend` and fall back to
/// `TokioFsBackend` if the ring cannot be initialized.
#[derive(Clone)]
pub enum AutoFsBackend {
    Tokio(TokioFsBackend),
    #[cfg(all(target_os = "linux", feature = "uring"))]
    Uring(uring::UringBackend),
}

pub enum AutoFsTransaction {
    Tokio(TokioFsTransaction),
    #[cfg(all(target_os = "linux", feature = "uring"))]
    Uring(uring::UringTransaction),
}

impl AutoFsBackend {
    pub async fn open<P: AsRef<std::path::Path>>(root: P) -> Result<Self> {
        #[cfg(all(target_os = "linux", feature = "uring"))]
        {
            match uring::UringBackend::open(root.as_ref()).await {
                Ok(backend) => return Ok(Self::Uring(backend)),
                Err(err) => {
                    tracing::info!(error = %err, "io_uring unavailable; falling back to tokio fs backend");
                }
            }
        }

        Ok(Self::Tokio(TokioFsBackend::open(root).await?))
    }

    /// The store id in `root/store`. The capsule journal is paired with it.
    pub fn store_id(&self) -> uuid::Uuid {
        match self {
            Self::Tokio(backend) => backend.store_id(),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.store_id(),
        }
    }

    /// One past the highest segment id with a file.
    pub fn segment_id_floor(&self) -> Result<u64> {
        match self {
            Self::Tokio(backend) => backend.segment_id_floor(),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.segment_id_floor(),
        }
    }

    /// Set every segment's ref count from `counts`; see
    /// [`TokioFsBackend::recount_references`].
    pub fn recount_references(&self, counts: &HashMap<SegmentId, u32>) -> Result<RecountReport> {
        match self {
            Self::Tokio(backend) => backend.recount_references(counts),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.recount_references(counts),
        }
    }
}

impl StorageTransaction for AutoFsTransaction {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        match self {
            Self::Tokio(txn) => txn.append(segment, data),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(txn) => txn.append(segment, data),
        }
    }

    fn set_segment_metadata<'a>(
        &'a mut self,
        segment: SegmentId,
        metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        match self {
            Self::Tokio(txn) => txn.set_segment_metadata(segment, metadata),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(txn) => txn.set_segment_metadata(segment, metadata),
        }
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        match self {
            Self::Tokio(txn) => txn.delete(segment),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(txn) => txn.delete(segment),
        }
    }

    fn commit(self) -> BoxFuture<'static, Result<()>>
    where
        Self: Sized,
    {
        match self {
            Self::Tokio(txn) => txn.commit(),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(txn) => txn.commit(),
        }
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>>
    where
        Self: Sized,
    {
        match self {
            Self::Tokio(txn) => txn.rollback(),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(txn) => txn.rollback(),
        }
    }
}

impl StorageBackend for AutoFsBackend {
    type Transaction = AutoFsTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        match self {
            Self::Tokio(backend) => backend.append(segment, data),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.append(segment, data),
        }
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        match self {
            Self::Tokio(backend) => backend.read(segment),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.read(segment),
        }
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        match self {
            Self::Tokio(backend) => backend.metadata(segment),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.metadata(segment),
        }
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        match self {
            Self::Tokio(backend) => backend.delete(segment),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.delete(segment),
        }
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        match self {
            Self::Tokio(backend) => backend.segment_ids(),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.segment_ids(),
        }
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        match self {
            Self::Tokio(backend) => Box::pin(async move {
                let txn = backend.begin_txn().await?;
                Ok(AutoFsTransaction::Tokio(txn))
            }),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => Box::pin(async move {
                let txn = backend.begin_txn().await?;
                Ok(AutoFsTransaction::Uring(txn))
            }),
        }
    }

    fn verify_stored(&self, segment: SegmentId) -> BoxFuture<'_, Result<Option<bool>>> {
        match self {
            Self::Tokio(backend) => backend.verify_stored(segment),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(backend) => backend.verify_stored(segment),
        }
    }
}

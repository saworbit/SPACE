use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use common::{
    traits::{StorageBackend, StorageTransaction},
    Segment, SegmentId,
};
use futures::future::{self, BoxFuture};
use nvram_sim::{NvramLog, NvramTransaction};
use tokio::io::AsyncWriteExt;

use tiering::{migrate_segment_to_cold, recall_segment_from_cold, TieringPaths};

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
            let mut guard = self.inner.lock().expect("in-memory backend mutex poisoned");
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
            let mut guard = inner.lock().expect("in-memory backend mutex poisoned");
            guard.segments.insert(segment, payload);
            Ok(())
        })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().expect("in-memory backend mutex poisoned");
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
            let guard = inner.lock().expect("in-memory backend mutex poisoned");
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
            let mut guard = inner.lock().expect("in-memory backend mutex poisoned");
            guard.segments.remove(&segment);
            guard.metadata.remove(&segment);
            Ok(())
        })
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = inner.lock().expect("in-memory backend mutex poisoned");
            Ok(guard.metadata.keys().copied().collect())
        })
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(future::ready(Ok(InMemoryTransaction::new(inner))))
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
}

/// Filesystem-backed storage backend.
///
/// Layout:
/// - `root/segments/<id>.bin` data payload (hot)
/// - `root/segments/<id>.stub.json` redirect stub (cold)
/// - `root/metadata/<id>.json` segment metadata
#[derive(Clone)]
pub struct TokioFsBackend {
    root: Arc<std::path::PathBuf>,
    tiering: Option<Arc<TieringPaths>>,
    reheat_on_read: bool,
}

impl TokioFsBackend {
    pub async fn open<P: AsRef<std::path::Path>>(root: P) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join("segments")).await?;
        tokio::fs::create_dir_all(root.join("metadata")).await?;
        Ok(Self {
            root: Arc::new(root),
            tiering: None,
            reheat_on_read: false,
        })
    }

    pub fn with_tiering(mut self, cold_root: std::path::PathBuf, reheat_on_read: bool) -> Self {
        let paths = TieringPaths {
            hot_root: (*self.root).clone(),
            cold_root,
        };
        self.tiering = Some(Arc::new(paths));
        self.reheat_on_read = reheat_on_read;
        self
    }

    fn metadata_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("metadata")
            .join(format!("{}.json", segment.0))
    }

    fn data_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.bin", segment.0))
    }

    fn stub_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.stub.json", segment.0))
    }

    async fn read_bytes(&self, segment: SegmentId) -> Result<Vec<u8>> {
        let data_path = self.data_path(segment);
        if tokio::fs::try_exists(&data_path).await.unwrap_or(false) {
            return Ok(tokio::fs::read(&data_path).await?);
        }

        if let Some(paths) = &self.tiering {
            return recall_segment_from_cold(paths, segment, self.reheat_on_read).await;
        }

        let stub_path = self.stub_path(segment);
        if tokio::fs::try_exists(&stub_path).await.unwrap_or(false) {
            return Err(anyhow!(
                "segment {} is cold but tiering is not configured",
                segment.0
            ));
        }

        Err(anyhow!("segment {:?} not found", segment))
    }

    pub async fn migrate_to_cold(&self, segment: SegmentId) -> Result<()> {
        let paths = self
            .tiering
            .as_ref()
            .ok_or_else(|| anyhow!("tiering not configured on backend"))?;
        migrate_segment_to_cold(paths, segment).await
    }
}

pub struct TokioFsTransaction {
    root: Arc<std::path::PathBuf>,
    tiering: Option<Arc<TieringPaths>>,
    staged_segments: HashMap<SegmentId, Vec<u8>>,
    staged_metadata: HashMap<SegmentId, Segment>,
    deleted: Vec<SegmentId>,
}

impl TokioFsTransaction {
    fn new(root: Arc<std::path::PathBuf>, tiering: Option<Arc<TieringPaths>>) -> Self {
        Self {
            root,
            tiering,
            staged_segments: HashMap::new(),
            staged_metadata: HashMap::new(),
            deleted: Vec::new(),
        }
    }

    fn data_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.bin", segment.0))
    }

    fn stub_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.stub.json", segment.0))
    }

    fn metadata_path(&self, segment: SegmentId) -> std::path::PathBuf {
        self.root
            .join("metadata")
            .join(format!("{}.json", segment.0))
    }

    async fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let tmp = path.with_extension(format!(
            "{}.tmp",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut file = tokio::fs::File::create(&tmp).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        drop(file);

        let _ = tokio::fs::remove_file(path).await;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    async fn delete_segment_files(&self, segment: SegmentId) -> Result<()> {
        let _ = tokio::fs::remove_file(self.data_path(segment)).await;
        let _ = tokio::fs::remove_file(self.stub_path(segment)).await;
        let _ = tokio::fs::remove_file(self.metadata_path(segment)).await;

        if let Some(paths) = &self.tiering {
            let _ = tokio::fs::remove_file(paths.cold_object_path(segment)).await;
        }

        Ok(())
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

    fn commit(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let root = Arc::clone(&self.root);
            let tiering = self.tiering.clone();

            for (segment, data) in self.staged_segments {
                let path = root.join("segments").join(format!("{}.bin", segment.0));
                Self::atomic_write(&path, &data).await?;
                let _ = tokio::fs::remove_file(
                    root.join("segments")
                        .join(format!("{}.stub.json", segment.0)),
                )
                .await;
            }

            for (segment, metadata) in self.staged_metadata {
                let path = root.join("metadata").join(format!("{}.json", segment.0));
                let bytes = serde_json::to_vec_pretty(&metadata)?;
                Self::atomic_write(&path, &bytes).await?;
            }

            for segment in self.deleted {
                let _ = tokio::fs::remove_file(
                    root.join("segments").join(format!("{}.bin", segment.0)),
                )
                .await;
                let _ = tokio::fs::remove_file(
                    root.join("segments")
                        .join(format!("{}.stub.json", segment.0)),
                )
                .await;
                let _ = tokio::fs::remove_file(
                    root.join("metadata").join(format!("{}.json", segment.0)),
                )
                .await;

                if let Some(paths) = &tiering {
                    let _ = tokio::fs::remove_file(paths.cold_object_path(segment)).await;
                }
            }

            Ok(())
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl StorageBackend for TokioFsBackend {
    type Transaction = TokioFsTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let root = Arc::clone(&self.root);
        let payload = data.to_vec();
        Box::pin(async move {
            tokio::fs::create_dir_all(root.join("segments")).await.ok();
            let path = root.join("segments").join(format!("{}.bin", segment.0));
            TokioFsTransaction::atomic_write(&path, &payload).await?;
            Ok(())
        })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        Box::pin(async move { self.read_bytes(segment).await })
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        let path = self.metadata_path(segment);
        Box::pin(async move {
            let bytes = tokio::fs::read(&path).await?;
            Ok(serde_json::from_slice(&bytes)?)
        })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        let root = Arc::clone(&self.root);
        let tiering = self.tiering.clone();
        Box::pin(async move {
            let txn = TokioFsTransaction::new(root, tiering);
            txn.delete_segment_files(segment).await?;
            Ok(())
        })
    }

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>> {
        let root = Arc::clone(&self.root);
        Box::pin(async move {
            let mut out = Vec::new();
            let mut dir = tokio::fs::read_dir(root.join("metadata")).await?;
            while let Some(entry) = dir.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(id) = stem.parse::<u64>() {
                        out.push(SegmentId(id));
                    }
                }
            }
            Ok(out)
        })
    }

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>> {
        let root = Arc::clone(&self.root);
        let tiering = self.tiering.clone();
        Box::pin(future::ready(Ok(TokioFsTransaction::new(root, tiering))))
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

    pub fn with_tiering(self, cold_root: std::path::PathBuf, reheat_on_read: bool) -> Self {
        match self {
            Self::Tokio(b) => Self::Tokio(b.with_tiering(cold_root, reheat_on_read)),
            #[cfg(all(target_os = "linux", feature = "uring"))]
            Self::Uring(b) => Self::Uring(b.with_tiering(cold_root, reheat_on_read)),
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
}

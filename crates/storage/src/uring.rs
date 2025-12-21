use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use common::traits::{StorageBackend, StorageTransaction};
use common::{Segment, SegmentId};
use futures::future::{self, BoxFuture};
use tokio::sync::{mpsc, oneshot};

use tiering::{migrate_segment_to_cold, recall_segment_from_cold, TieringPaths};

#[derive(Debug)]
enum UringCommand {
    Read {
        path: PathBuf,
        len: usize,
        resp: oneshot::Sender<Result<Vec<u8>>>,
    },
    Write {
        path: PathBuf,
        data: Vec<u8>,
        resp: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

/// io_uring-backed filesystem storage backend (Linux-only).
///
/// Uses the same on-disk layout as `TokioFsBackend` so tiering stubs remain compatible.
#[derive(Clone)]
pub struct UringBackend {
    command_tx: mpsc::UnboundedSender<UringCommand>,
    root: Arc<PathBuf>,
    tiering: Option<Arc<TieringPaths>>,
    reheat_on_read: bool,
}

impl UringBackend {
    pub async fn open<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join("segments")).await?;
        tokio::fs::create_dir_all(root.join("metadata")).await?;

        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let _ = tokio_uring::start(async move {
                run_uring_actor(rx).await;
            });
        });

        Ok(Self {
            command_tx: tx,
            root: Arc::new(root),
            tiering: None,
            reheat_on_read: false,
        })
    }

    pub fn with_tiering(mut self, cold_root: PathBuf, reheat_on_read: bool) -> Self {
        let paths = TieringPaths {
            hot_root: (*self.root).clone(),
            cold_root,
        };
        self.tiering = Some(Arc::new(paths));
        self.reheat_on_read = reheat_on_read;
        self
    }

    fn metadata_path(&self, segment: SegmentId) -> PathBuf {
        self.root
            .join("metadata")
            .join(format!("{}.json", segment.0))
    }

    fn data_path(&self, segment: SegmentId) -> PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.bin", segment.0))
    }

    fn stub_path(&self, segment: SegmentId) -> PathBuf {
        self.root
            .join("segments")
            .join(format!("{}.stub.json", segment.0))
    }

    async fn read_len_hint(&self, segment: SegmentId) -> Option<usize> {
        let meta_path = self.metadata_path(segment);
        let bytes = tokio::fs::read(&meta_path).await.ok()?;
        let seg: Segment = serde_json::from_slice(&bytes).ok()?;
        Some(seg.len as usize)
    }

    async fn read_bytes(&self, segment: SegmentId) -> Result<Vec<u8>> {
        let data_path = self.data_path(segment);
        if tokio::fs::try_exists(&data_path).await.unwrap_or(false) {
            let len = self
                .read_len_hint(segment)
                .await
                .or_else(|| std::fs::metadata(&data_path).ok().map(|m| m.len() as usize))
                .unwrap_or(0);

            let (resp_tx, resp_rx) = oneshot::channel();
            self.command_tx
                .send(UringCommand::Read {
                    path: data_path,
                    len,
                    resp: resp_tx,
                })
                .map_err(|_| anyhow!("io_uring actor is not running"))?;
            return resp_rx
                .await
                .map_err(|_| anyhow!("io_uring actor dropped response"))??;
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

    async fn write_bytes(&self, segment: SegmentId, data: Vec<u8>) -> Result<()> {
        let path = self.data_path(segment);
        let (resp_tx, resp_rx) = oneshot::channel();
        self.command_tx
            .send(UringCommand::Write {
                path,
                data,
                resp: resp_tx,
            })
            .map_err(|_| anyhow!("io_uring actor is not running"))?;
        resp_rx
            .await
            .map_err(|_| anyhow!("io_uring actor dropped response"))??;

        let _ = tokio::fs::remove_file(self.stub_path(segment)).await;
        Ok(())
    }

    pub async fn migrate_to_cold(&self, segment: SegmentId) -> Result<()> {
        let paths = self
            .tiering
            .as_ref()
            .ok_or_else(|| anyhow!("tiering not configured on backend"))?;
        migrate_segment_to_cold(paths, segment).await
    }
}

pub struct UringTransaction {
    backend: UringBackend,
    staged_segments: HashMap<SegmentId, Vec<u8>>,
    staged_metadata: HashMap<SegmentId, Segment>,
    deleted: Vec<SegmentId>,
}

impl UringTransaction {
    fn new(backend: UringBackend) -> Self {
        Self {
            backend,
            staged_segments: HashMap::new(),
            staged_metadata: HashMap::new(),
            deleted: Vec::new(),
        }
    }

    async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
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
        tokio::fs::write(&tmp, bytes).await?;
        let _ = tokio::fs::remove_file(path).await;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    async fn delete_segment_files(&self, segment: SegmentId) -> Result<()> {
        let _ = tokio::fs::remove_file(self.backend.data_path(segment)).await;
        let _ = tokio::fs::remove_file(self.backend.stub_path(segment)).await;
        let _ = tokio::fs::remove_file(self.backend.metadata_path(segment)).await;
        if let Some(paths) = &self.backend.tiering {
            let _ = tokio::fs::remove_file(paths.cold_object_path(segment)).await;
        }
        Ok(())
    }
}

impl StorageTransaction for UringTransaction {
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
            for (segment, data) in self.staged_segments {
                self.backend.write_bytes(segment, data).await?;
            }

            for (segment, metadata) in self.staged_metadata {
                let path = self.backend.metadata_path(segment);
                let bytes = serde_json::to_vec_pretty(&metadata)?;
                Self::atomic_write(&path, &bytes).await?;
            }

            for segment in self.deleted {
                self.delete_segment_files(segment).await?;
            }

            Ok(())
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl StorageBackend for UringBackend {
    type Transaction = UringTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let data_vec = data.to_vec();
        Box::pin(async move { self.write_bytes(segment, data_vec).await })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        Box::pin(async move { self.read_bytes(segment).await })
    }

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>> {
        let path = self.metadata_path(segment);
        Box::pin(async move {
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read segment metadata {}", path.display()))?;
            Ok(serde_json::from_slice(&bytes)?)
        })
    }

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        let backend = self.clone();
        Box::pin(async move {
            let txn = UringTransaction::new(backend);
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
        let backend = self.clone();
        Box::pin(future::ready(Ok(UringTransaction::new(backend))))
    }
}

async fn run_uring_actor(mut rx: mpsc::UnboundedReceiver<UringCommand>) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            UringCommand::Read { path, len, resp } => {
                let result = read_exact_len(&path, len).await;
                let _ = resp.send(result);
            }
            UringCommand::Write { path, data, resp } => {
                let result = write_atomic(&path, data).await;
                let _ = resp.send(result);
            }
            UringCommand::Shutdown => break,
        }
    }
}

async fn read_exact_len(path: &Path, len: usize) -> Result<Vec<u8>> {
    if len == 0 {
        let file = tokio_uring::fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let buffer = Vec::<u8>::with_capacity(0);
        let (res, _buf) = file.read_at(buffer, 0).await;
        let _ = res?;
        return Ok(Vec::new());
    }

    let file = tokio_uring::fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let buffer = vec![0u8; len];
    let (res, buffer) = file.read_at(buffer, 0).await;
    let bytes_read = res?;
    Ok(buffer[..bytes_read].to_vec())
}

async fn write_atomic(path: &Path, data: Vec<u8>) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let file = tokio_uring::fs::File::create(&tmp)
        .await
        .with_context(|| format!("create {}", tmp.display()))?;
    let len = data.len();
    let (res, _buf) = file.write_at(data, 0).await;
    let written = res?;
    if written != len {
        return Err(anyhow!("short write: {} != {}", written, len));
    }
    file.sync_data()
        .await
        .with_context(|| format!("sync {}", tmp.display()))?;
    file.close().await.ok();

    tokio_uring::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

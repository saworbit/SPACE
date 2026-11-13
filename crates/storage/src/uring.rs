use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use common::traits::{StorageBackend, StorageTransaction};
use common::{Segment, SegmentId};
use futures::future::{self, BoxFuture};
use nvram_sim::RecountReport;
use tokio::sync::{mpsc, oneshot};

use crate::fs_root::{on_root, FsRoot};

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
}

/// io_uring-backed filesystem storage backend (Linux-only).
///
/// Uses the same on-disk layout as `TokioFsBackend` (`docs/on-disk-format.md`
/// §10); io_uring reads and writes the segment files.
#[derive(Clone)]
pub struct UringBackend {
    command_tx: mpsc::UnboundedSender<UringCommand>,
    root: Arc<FsRoot>,
}

impl UringBackend {
    pub async fn open<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let root = tokio::task::spawn_blocking(move || FsRoot::open(&root)).await??;

        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            // Runs until every sender is dropped.
            tokio_uring::start(async move {
                run_uring_actor(rx).await;
            });
        });

        Ok(Self {
            command_tx: tx,
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

    /// Set every segment's ref count from `counts`.
    pub fn recount_references(&self, counts: &HashMap<SegmentId, u32>) -> Result<RecountReport> {
        self.root.recount_references(counts)
    }

    async fn read_bytes(&self, segment: SegmentId) -> Result<Vec<u8>> {
        let path = self.root.segment_path(segment);
        let len = match std::fs::metadata(&path) {
            Ok(meta) => meta.len() as usize,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("Segment not found: {segment:?}"));
            }
            Err(err) => return Err(err.into()),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        self.command_tx
            .send(UringCommand::Read {
                path,
                len,
                resp: resp_tx,
            })
            .map_err(|_| anyhow!("io_uring actor is not running"))?;
        let file = resp_rx
            .await
            .map_err(|_| anyhow!("io_uring actor dropped response"))??;
        FsRoot::body(segment, file)
    }

    /// Write `segment`'s file, with `metadata`'s descriptor, and set its ref
    /// count.
    async fn write_segment(
        &self,
        segment: SegmentId,
        data: &[u8],
        metadata: Option<&Segment>,
    ) -> Result<()> {
        let bytes = FsRoot::encode_segment(segment, data, metadata)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.command_tx
            .send(UringCommand::Write {
                path: self.root.segment_path(segment),
                data: bytes,
                resp: resp_tx,
            })
            .map_err(|_| anyhow!("io_uring actor is not running"))?;
        resp_rx
            .await
            .map_err(|_| anyhow!("io_uring actor dropped response"))??;
        self.root.wrote_segment(segment, metadata);
        Ok(())
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

    fn commit(mut self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            for (segment, data) in std::mem::take(&mut self.staged_segments) {
                let metadata = self.staged_metadata.remove(&segment);
                self.backend
                    .write_segment(segment, &data, metadata.as_ref())
                    .await?;
            }

            let root = Arc::clone(&self.backend.root);
            let metadata = std::mem::take(&mut self.staged_metadata);
            let deleted = std::mem::take(&mut self.deleted);
            on_root(&root, move |root| {
                root.commit(HashMap::new(), metadata, deleted)
            })
            .await
        })
    }

    fn rollback(self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl StorageBackend for UringBackend {
    type Transaction = UringTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.write_segment(segment, data, None).await })
    }

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>> {
        Box::pin(async move { self.read_bytes(segment).await })
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
        let backend = self.clone();
        Box::pin(future::ready(Ok(UringTransaction::new(backend))))
    }

    /// Each segment file's record checksums its descriptor and stored bytes.
    fn verify_stored(&self, segment: SegmentId) -> BoxFuture<'_, Result<Option<bool>>> {
        on_root(&self.root, move |root| root.verify(segment).map(Some))
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

/// Write `data` to `path` so it's whole or absent: to `<path>.tmp`, synced,
/// then renamed over `path`. The caller syncs the directory.
async fn write_atomic(path: &Path, data: Vec<u8>) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    let file = tokio_uring::fs::File::create(&tmp)
        .await
        .with_context(|| format!("create {}", tmp.display()))?;
    let len = data.len();
    let (res, _buf) = file.write_at(data, 0).submit().await;
    let written = res?;
    if written != len {
        return Err(anyhow!("short write: {} != {}", written, len));
    }
    file.sync_all()
        .await
        .with_context(|| format!("sync {}", tmp.display()))?;
    file.close().await.ok();

    tokio_uring::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

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

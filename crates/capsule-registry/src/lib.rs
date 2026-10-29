use anyhow::{anyhow, Result};

use common::*;
use std::path::Path;
use std::sync::Arc;

use crate::metadata_log::MetadataLog;
use crate::metadata_ops::{MetadataOp, OpResult};
use crate::store::{MetadataStore, SledStore};

mod metadata_log;
mod metadata_ops;
mod store;

const DEFAULT_PAGE_SIZE: usize = 1024;

pub mod dedup; // NEW
pub mod error;
pub mod pipeline;

pub mod scrub_executor;

pub use error::{CompressionError, DedupError, PipelineError};

pub mod modular_pipeline {
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use common::{traits::DataStream, CapsuleId, EncryptionPolicy, Policy};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    pub use pipeline::{
        pipeline_with_nvram, pipeline_with_nvram_xts, CapsuleWrite, DefaultPipeline,
        DefaultPolicyEvaluator, InMemoryPipeline, KeyManagerKeyring, NoopEncryptor, NullKeyring,
        NvramPipeline, NvramPipelineWithEncryption, Pipeline, PipelineBuilder, XtsEncryptor,
    };
    pub use storage::{AutoFsBackend, InMemoryBackend, NvramBackend};
    use tokio::io::{AsyncRead, AsyncReadExt};
    use tokio::sync::Mutex as TokioMutex;
    use tracing::warn;

    pub fn nvram_pipeline_with_encryption<P: AsRef<std::path::Path>>(
        path: P,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<NvramPipelineWithEncryption> {
        pipeline_with_nvram_xts(path, key_manager)
    }

    pub type RegistryEncryptedPipeline = Pipeline<
        compression::Lz4ZstdCompressor,
        dedup::Blake3Deduper,
        XtsEncryptor,
        NvramBackend,
        DefaultPolicyEvaluator,
        KeyManagerKeyring,
        crate::CapsuleRegistry,
    >;

    pub type RegistryPlainPipeline = Pipeline<
        compression::Lz4ZstdCompressor,
        dedup::Blake3Deduper,
        NoopEncryptor,
        NvramBackend,
        DefaultPolicyEvaluator,
        NullKeyring,
        crate::CapsuleRegistry,
    >;

    pub type RegistryFsEncryptedPipeline = Pipeline<
        compression::Lz4ZstdCompressor,
        dedup::Blake3Deduper,
        XtsEncryptor,
        AutoFsBackend,
        DefaultPolicyEvaluator,
        KeyManagerKeyring,
        crate::CapsuleRegistry,
    >;

    pub type RegistryFsPlainPipeline = Pipeline<
        compression::Lz4ZstdCompressor,
        dedup::Blake3Deduper,
        NoopEncryptor,
        AutoFsBackend,
        DefaultPolicyEvaluator,
        NullKeyring,
        crate::CapsuleRegistry,
    >;

    pub enum RegistryPipelineHandle {
        Encrypted(RegistryEncryptedPipeline),
        Plain(RegistryPlainPipeline),
        FsEncrypted(RegistryFsEncryptedPipeline),
        FsPlain(RegistryFsPlainPipeline),
    }

    impl RegistryPipelineHandle {
        /// The registry this pipeline records capsules in.
        pub fn registry(&self) -> &crate::CapsuleRegistry {
            match self {
                Self::Encrypted(p) => p.catalog(),
                Self::Plain(p) => p.catalog(),
                Self::FsEncrypted(p) => p.catalog(),
                Self::FsPlain(p) => p.catalog(),
            }
        }

        /// Policy for writes that don't name one. With a master key
        /// configured, that is the default policy with encryption on.
        pub fn default_policy(&self) -> Policy {
            match self {
                Self::Encrypted(_) | Self::FsEncrypted(_) => Policy {
                    encryption: EncryptionPolicy::XtsAes256 { key_version: None },
                    ..Policy::default()
                },
                Self::Plain(_) | Self::FsPlain(_) => Policy::default(),
            }
        }

        pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
            policy.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
            match self {
                Self::Encrypted(p) => p.write_capsule(data, policy).await,
                Self::Plain(p) => p.write_capsule(data, policy).await,
                Self::FsEncrypted(p) => p.write_capsule(data, policy).await,
                Self::FsPlain(p) => p.write_capsule(data, policy).await,
            }
        }

        pub fn begin_write(&mut self, policy: &Policy) -> Result<CapsuleWrite> {
            match self {
                Self::Encrypted(p) => p.begin_write(policy),
                Self::Plain(p) => p.begin_write(policy),
                Self::FsEncrypted(p) => p.begin_write(policy),
                Self::FsPlain(p) => p.begin_write(policy),
            }
        }

        pub async fn write_chunk(&mut self, write: &mut CapsuleWrite, chunk: &[u8]) -> Result<()> {
            match self {
                Self::Encrypted(p) => p.write_chunk(write, chunk).await,
                Self::Plain(p) => p.write_chunk(write, chunk).await,
                Self::FsEncrypted(p) => p.write_chunk(write, chunk).await,
                Self::FsPlain(p) => p.write_chunk(write, chunk).await,
            }
        }

        pub async fn finish_write(&mut self, write: CapsuleWrite) -> Result<CapsuleId> {
            match self {
                Self::Encrypted(p) => p.finish_write(write).await,
                Self::Plain(p) => p.finish_write(write).await,
                Self::FsEncrypted(p) => p.finish_write(write).await,
                Self::FsPlain(p) => p.finish_write(write).await,
            }
        }

        pub async fn abort_write(&mut self, write: CapsuleWrite) -> Result<()> {
            match self {
                Self::Encrypted(p) => p.abort_write(write).await,
                Self::Plain(p) => p.abort_write(write).await,
                Self::FsEncrypted(p) => p.abort_write(write).await,
                Self::FsPlain(p) => p.abort_write(write).await,
            }
        }

        pub async fn read_range_stream(
            &self,
            id: CapsuleId,
            offset: u64,
            len: u64,
        ) -> Result<DataStream> {
            match self {
                Self::Encrypted(p) => p.read_range_stream(id, offset, len).await,
                Self::Plain(p) => p.read_range_stream(id, offset, len).await,
                Self::FsEncrypted(p) => p.read_range_stream(id, offset, len).await,
                Self::FsPlain(p) => p.read_range_stream(id, offset, len).await,
            }
        }

        pub async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
            match self {
                Self::Encrypted(p) => p.read_capsule(id).await,
                Self::Plain(p) => p.read_capsule(id).await,
                Self::FsEncrypted(p) => p.read_capsule(id).await,
                Self::FsPlain(p) => p.read_capsule(id).await,
            }
        }

        pub async fn read_capsule_stream(&self, id: CapsuleId) -> Result<DataStream> {
            match self {
                Self::Encrypted(p) => p.read_capsule_stream(id).await,
                Self::Plain(p) => p.read_capsule_stream(id).await,
                Self::FsEncrypted(p) => p.read_capsule_stream(id).await,
                Self::FsPlain(p) => p.read_capsule_stream(id).await,
            }
        }

        pub async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
            match self {
                Self::Encrypted(p) => p.read_range(id, offset, len).await,
                Self::Plain(p) => p.read_range(id, offset, len).await,
                Self::FsEncrypted(p) => p.read_range(id, offset, len).await,
                Self::FsPlain(p) => p.read_range(id, offset, len).await,
            }
        }

        pub async fn delete_capsule(&mut self, id: CapsuleId) -> Result<()> {
            match self {
                Self::Encrypted(p) => p.delete_capsule(id).await,
                Self::Plain(p) => p.delete_capsule(id).await,
                Self::FsEncrypted(p) => p.delete_capsule(id).await,
                Self::FsPlain(p) => p.delete_capsule(id).await,
            }
        }

        pub async fn garbage_collect(&mut self) -> Result<usize> {
            match self {
                Self::Encrypted(p) => p.garbage_collect().await,
                Self::Plain(p) => p.garbage_collect().await,
                Self::FsEncrypted(p) => p.garbage_collect().await,
                Self::FsPlain(p) => p.garbage_collect().await,
            }
        }
    }

    /// Write a capsule from `reader` one segment at a time, so memory stays
    /// near one segment however long the input is (#99). The pipeline is
    /// locked only while a segment is stored, never while the reader waits.
    /// If this future is dropped part way, the write is aborted in the
    /// background. Returns the capsule and its size.
    pub async fn write_from_reader<Rd>(
        pipeline: &Arc<TokioMutex<RegistryPipelineHandle>>,
        reader: Rd,
        policy: &Policy,
    ) -> Result<(CapsuleId, u64)>
    where
        Rd: AsyncRead + Send,
    {
        let write = pipeline.lock().await.begin_write(policy)?;
        let chunk_size = write.chunk_size();
        let mut open = OpenWrite {
            pipeline: Arc::clone(pipeline),
            write: Some(write),
        };
        tokio::pin!(reader);
        let mut chunk = Vec::with_capacity(chunk_size);
        let mut size = 0u64;
        loop {
            chunk.clear();
            if let Err(err) = reader
                .as_mut()
                .take(chunk_size as u64)
                .read_to_end(&mut chunk)
                .await
            {
                open.abort().await;
                return Err(err.into());
            }
            if chunk.is_empty() {
                break;
            }
            size += chunk.len() as u64;
            let stored = pipeline
                .lock()
                .await
                .write_chunk(open.write_mut(), &chunk)
                .await;
            if let Err(err) = stored {
                open.abort().await;
                return Err(err);
            }
            if chunk.len() < chunk_size {
                break;
            }
        }

        let mut handle = pipeline.lock().await;
        let write = open.take();
        Ok((handle.finish_write(write).await?, size))
    }

    /// Aborts the write in the background if dropped before it ends.
    struct OpenWrite {
        pipeline: Arc<TokioMutex<RegistryPipelineHandle>>,
        write: Option<CapsuleWrite>,
    }

    impl OpenWrite {
        fn write_mut(&mut self) -> &mut CapsuleWrite {
            self.write.as_mut().expect("write is open")
        }

        fn take(&mut self) -> CapsuleWrite {
            self.write.take().expect("write is open")
        }

        async fn abort(mut self) {
            let write = self.take();
            if let Err(err) = self.pipeline.lock().await.abort_write(write).await {
                warn!("failed to release the segments of an aborted write: {err:#}");
            }
        }
    }

    impl Drop for OpenWrite {
        fn drop(&mut self) {
            let Some(write) = self.write.take() else {
                return;
            };
            let pipeline = Arc::clone(&self.pipeline);
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    runtime.spawn(async move {
                        if let Err(err) = pipeline.lock().await.abort_write(write).await {
                            warn!("failed to release the segments of a dropped write: {err:#}");
                        }
                    });
                }
                Err(_) => warn!("a write was dropped outside a runtime; its segments stay"),
            }
        }
    }

    pub fn registry_pipeline_from_env<P: AsRef<std::path::Path>>(
        path: P,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        let backend = NvramBackend::open(path)?;
        registry_pipeline_from_nvram_backend(backend, registry)
    }

    pub fn registry_pipeline_from_log(
        log: NvramLog,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        let backend = NvramBackend::from_log(log);
        registry_pipeline_from_nvram_backend(backend, registry)
    }

    pub fn registry_pipeline_from_log_with_key(
        log: NvramLog,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryPipelineHandle> {
        let backend = NvramBackend::from_log(log);
        let pipeline = build_nvram_encrypted_pipeline(backend, registry, key_manager)?;
        Ok(RegistryPipelineHandle::Encrypted(pipeline))
    }

    pub fn registry_nvram_pipeline_with_encryption<P: AsRef<std::path::Path>>(
        path: P,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryEncryptedPipeline> {
        let storage = NvramBackend::open(path)?;
        build_nvram_encrypted_pipeline(storage, registry, key_manager)
    }

    pub async fn registry_pipeline_from_storage_root<P: AsRef<std::path::Path>>(
        root: P,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        let storage = AutoFsBackend::open(root).await?;
        registry_pipeline_from_fs_backend(storage, registry)
    }

    fn registry_pipeline_from_nvram_backend(
        storage: NvramBackend,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        // A set but invalid SPACE_MASTER_KEY is an error, not a quiet
        // fallback to the plain pipeline (#84).
        if let Some(manager) = KeyManager::from_env_if_set()? {
            let km = Arc::new(Mutex::new(manager));
            let pipeline = build_nvram_encrypted_pipeline(storage, registry, km)?;
            Ok(RegistryPipelineHandle::Encrypted(pipeline))
        } else {
            Ok(RegistryPipelineHandle::Plain(Pipeline::new(
                compression::Lz4ZstdCompressor,
                dedup::Blake3Deduper::default(),
                NoopEncryptor,
                storage,
                DefaultPolicyEvaluator,
                None,
                registry,
            )))
        }
    }

    pub fn registry_pipeline_from_fs_backend(
        storage: AutoFsBackend,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        // A set but invalid SPACE_MASTER_KEY is an error, not a quiet
        // fallback to the plain pipeline (#84).
        if let Some(manager) = KeyManager::from_env_if_set()? {
            let km = Arc::new(Mutex::new(manager));
            let pipeline = build_fs_encrypted_pipeline(storage, registry, km)?;
            Ok(RegistryPipelineHandle::FsEncrypted(pipeline))
        } else {
            Ok(RegistryPipelineHandle::FsPlain(Pipeline::new(
                compression::Lz4ZstdCompressor,
                dedup::Blake3Deduper::default(),
                NoopEncryptor,
                storage,
                DefaultPolicyEvaluator,
                None,
                registry,
            )))
        }
    }

    fn build_nvram_encrypted_pipeline(
        storage: NvramBackend,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryEncryptedPipeline> {
        Ok(Pipeline::new(
            compression::Lz4ZstdCompressor,
            dedup::Blake3Deduper::default(),
            XtsEncryptor::new(Arc::clone(&key_manager)),
            storage,
            DefaultPolicyEvaluator,
            Some(KeyManagerKeyring::new(key_manager)),
            registry,
        ))
    }

    fn build_fs_encrypted_pipeline(
        storage: AutoFsBackend,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryFsEncryptedPipeline> {
        Ok(Pipeline::new(
            compression::Lz4ZstdCompressor,
            dedup::Blake3Deduper::default(),
            XtsEncryptor::new(Arc::clone(&key_manager)),
            storage,
            DefaultPolicyEvaluator,
            Some(KeyManagerKeyring::new(key_manager)),
            registry,
        ))
    }
}

impl common::traits::CapsuleCatalog for CapsuleRegistry {
    fn allocate_segment(&self) -> Result<SegmentId> {
        self.alloc_segment()
    }

    fn lookup_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.lookup(id)
    }

    fn create_capsule(
        &self,
        id: CapsuleId,
        size: u64,
        policy: &Policy,
        segments: Vec<SegmentId>,
        stats: &common::traits::DedupStats,
    ) -> Result<()> {
        self.create_capsule_with_segments(id, size, segments, policy.clone())?;
        if stats.bytes_saved > 0 {
            self.add_deduped_bytes(id, stats.bytes_saved)?;
        }
        Ok(())
    }

    fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.delete_capsule(id)
    }

    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.lookup_content(hash)
    }

    fn register_content(&self, hash: ContentHash, segment: SegmentId) -> Result<()> {
        self.register_content(hash, segment)
    }

    fn deregister_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        self.deregister_content(hash, segment)
    }

    fn capsules(&self) -> Vec<Capsule> {
        self.capsules()
    }

    fn content_entries(&self) -> Vec<(ContentHash, SegmentId)> {
        self.content_entries()
    }
}

pub struct CapsuleRegistry {
    store: Arc<dyn MetadataStore>,
    log: MetadataLog,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.db").expect(
            "failed to open registry DB at 'space.db'; check disk permissions and available space",
        )
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let store: Arc<dyn MetadataStore> = Arc::new(SledStore::open(&path_str)?);
        let log = MetadataLog::new(Arc::clone(&store));

        Ok(Self { store, log })
    }

    fn collect_capsules_paginated(&self, page_size: usize) -> Result<Vec<Capsule>> {
        if page_size == 0 {
            return Ok(Vec::new());
        }

        let mut cursor: Option<CapsuleId> = None;
        let mut capsules = Vec::with_capacity(page_size);

        loop {
            let page = self.store.list_capsules(page_size, cursor)?;
            if page.is_empty() {
                break;
            }

            let page_len = page.len();
            cursor = page.last().map(|c| c.id);
            capsules.extend(page);

            if page_len < page_size {
                break;
            }
        }

        Ok(capsules)
    }

    fn insert_capsule(&self, capsule: Capsule) -> Result<()> {
        if self.store.get_capsule(&capsule.id)?.is_some() {
            anyhow::bail!("Capsule collision (extremely unlikely)");
        }
        match self.log.apply(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    /// Insert capsule metadata if it does not already exist.
    ///
    /// Returns `true` if newly inserted, or `false` if a capsule with the same ID
    /// is already present (idempotent insert).
    pub fn put_capsule(&self, capsule: Capsule) -> Result<bool> {
        if self.store.get_capsule(&capsule.id)?.is_some() {
            return Ok(false);
        }
        match self.log.apply(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(true),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn create_capsule_with_segments(
        &self,
        id: CapsuleId,
        size: u64,
        segments: Vec<SegmentId>,
        policy: Policy,
    ) -> Result<()> {
        let capsule = Capsule {
            id,
            size,
            segments,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            policy,
            deduped_bytes: 0,
        };

        self.insert_capsule(capsule)
    }

    /// Like `lookup`, but a missing capsule is `None` rather than an error.
    pub fn get_capsule(&self, id: CapsuleId) -> Result<Option<Capsule>> {
        self.store.get_capsule(&id)
    }

    pub fn lookup(&self, id: CapsuleId) -> Result<Capsule> {
        self.store
            .get_capsule(&id)?
            .ok_or_else(|| anyhow!("Capsule not found"))
    }

    pub fn serialize_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.lookup(id)?;
        Ok(serde_json::to_vec(&capsule)?)
    }

    pub fn alloc_segment(&self) -> Result<SegmentId> {
        self.store
            .allocate_segment_id()
            .map_err(|err| anyhow!("failed to allocate segment id: {err}"))
    }

    pub fn add_segment(&self, capsule_id: CapsuleId, seg_id: SegmentId) -> Result<()> {
        let mut capsule = self.lookup(capsule_id)?;
        capsule.segments.push(seg_id);
        match self.log.apply(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.store.get_content(hash).ok().flatten()
    }

    pub fn register_content(&self, hash: ContentHash, seg_id: SegmentId) -> Result<()> {
        match self.log.apply(MetadataOp::RegisterContent {
            hash: hash.clone(),
            segment: seg_id,
        })? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn deregister_content(&self, hash: &ContentHash, seg_id: SegmentId) -> Result<bool> {
        match self.log.apply(MetadataOp::DeregisterContent {
            hash: hash.clone(),
            segment: seg_id,
        })? {
            OpResult::Ok => Ok(true),
            OpResult::NotFound => Ok(false),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn add_deduped_bytes(&self, capsule_id: CapsuleId, bytes: u64) -> Result<()> {
        let mut capsule = match self.lookup(capsule_id) {
            Ok(capsule) => capsule,
            Err(_) => return Ok(()), // Capsule deleted before dedup update; treat as noop
        };
        capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
        match self.log.apply(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn list_capsules(&self, limit: usize, cursor: Option<CapsuleId>) -> Result<Vec<CapsuleId>> {
        let capsules = self.store.list_capsules(limit, cursor)?;
        Ok(capsules.into_iter().map(|c| c.id).collect())
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        match self.log.apply(MetadataOp::DeleteCapsule(id))? {
            OpResult::CapsuleFound(capsule) => Ok(capsule),
            OpResult::NotFound => Err(anyhow!("Capsule not found")),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    /// Create an S3 bucket. Creating one that exists is a no-op.
    pub fn create_bucket(&self, bucket: &str) -> Result<()> {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        match self.log.apply(MetadataOp::PutBucket {
            bucket: bucket.to_string(),
            created_at,
        })? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    /// When an S3 bucket was created, or `None` if it doesn't exist.
    pub fn bucket_created_at(&self, bucket: &str) -> Result<Option<u64>> {
        self.store.get_bucket(bucket)
    }

    /// The capsule an S3 key points at.
    pub fn object(&self, bucket: &str, key: &str) -> Result<Option<CapsuleId>> {
        self.store.get_object(bucket, key)
    }

    /// Point an S3 key at a capsule. Returns the capsule it pointed at before.
    pub fn bind_object(
        &self,
        bucket: &str,
        key: &str,
        capsule: CapsuleId,
    ) -> Result<Option<CapsuleId>> {
        self.replaced(MetadataOp::PutObject {
            bucket: bucket.to_string(),
            key: key.to_string(),
            capsule,
        })
    }

    /// Remove an S3 key. Returns the capsule it pointed at.
    pub fn unbind_object(&self, bucket: &str, key: &str) -> Result<Option<CapsuleId>> {
        self.replaced(MetadataOp::DeleteObject {
            bucket: bucket.to_string(),
            key: key.to_string(),
        })
    }

    /// S3 keys in `bucket` that start with `prefix` and sort after
    /// `start_after`, in byte order, at most `limit` of them.
    pub fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, CapsuleId)>> {
        self.store.list_objects(bucket, prefix, start_after, limit)
    }

    fn replaced(&self, op: MetadataOp) -> Result<Option<CapsuleId>> {
        match self.log.apply(op)? {
            OpResult::Replaced(previous) => Ok(previous),
            other => anyhow::bail!("unexpected metadata op result: {:?}", other),
        }
    }

    pub fn get_dedup_stats(&self) -> (usize, usize) {
        let content_store = self.store.list_content().unwrap_or_default();
        let capsules = self
            .collect_capsules_paginated(DEFAULT_PAGE_SIZE)
            .unwrap_or_default();

        let total_segments: usize = capsules.iter().map(|c| c.segments.len()).sum();
        let unique_segments = content_store.len();

        (total_segments, unique_segments)
    }

    pub fn capsules(&self) -> Vec<Capsule> {
        self.collect_capsules_paginated(DEFAULT_PAGE_SIZE)
            .unwrap_or_default()
    }

    pub fn content_entries(&self) -> Vec<(ContentHash, SegmentId)> {
        self.store.list_content().unwrap_or_default()
    }
}

impl Default for CapsuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CapsuleRegistry {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            log: self.log.clone(),
        }
    }
}

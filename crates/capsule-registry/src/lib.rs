use anyhow::{anyhow, bail, Result};

use common::format::{CapsuleManifest, JournalRecord};
use common::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::warn;

use crate::journal::{Journal, JournalPosition};
use crate::store::{MetadataStore, SledStore};

mod journal;
mod store;

const DEFAULT_PAGE_SIZE: usize = 1024;

pub mod dedup; // NEW
pub mod error;
pub mod pipeline;

pub mod scrub_executor;

pub use error::{CompressionError, DedupError, PipelineError};

pub mod modular_pipeline {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use common::format::JournalRecord;
    use common::{traits::DataStream, Capsule, CapsuleId, EncryptionPolicy, Policy, SegmentId};
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
    use tracing::{info, warn};

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

        /// Commit the write together with `with`, all or nothing. Returns
        /// the capsules `with` dropped; release each with [`Self::release`].
        pub async fn finish_write_with(
            &mut self,
            write: CapsuleWrite,
            with: Vec<JournalRecord>,
        ) -> Result<Vec<Capsule>> {
            match self {
                Self::Encrypted(p) => p.finish_write_with(write, with).await,
                Self::Plain(p) => p.finish_write_with(write, with).await,
                Self::FsEncrypted(p) => p.finish_write_with(write, with).await,
                Self::FsPlain(p) => p.finish_write_with(write, with).await,
            }
        }

        /// Commit `records`, all or nothing. Returns the capsules they
        /// dropped; release each with [`Self::release`].
        pub fn commit(&mut self, records: Vec<JournalRecord>) -> Result<Vec<Capsule>> {
            match self {
                Self::Encrypted(p) => p.commit(records),
                Self::Plain(p) => p.commit(records),
                Self::FsEncrypted(p) => p.commit(records),
                Self::FsPlain(p) => p.commit(records),
            }
        }

        /// Release a dropped capsule's references to its segments.
        pub async fn release(&mut self, capsule: &Capsule) -> Result<()> {
            match self {
                Self::Encrypted(p) => p.release(capsule).await,
                Self::Plain(p) => p.release(capsule).await,
                Self::FsEncrypted(p) => p.release(capsule).await,
                Self::FsPlain(p) => p.release(capsule).await,
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
        let staged = stage_from_reader(pipeline, reader, policy).await?;
        let (id, size) = (staged.capsule_id(), staged.size());
        let mut handle = pipeline.lock().await;
        staged.commit(&mut handle, Vec::new()).await?;
        Ok((id, size))
    }

    /// Store everything `reader` yields as an open write, as
    /// [`write_from_reader`] does, but leave committing it to the caller:
    /// see [`StagedWrite::commit`].
    pub async fn stage_from_reader<Rd>(
        pipeline: &Arc<TokioMutex<RegistryPipelineHandle>>,
        reader: Rd,
        policy: &Policy,
    ) -> Result<StagedWrite>
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
        Ok(StagedWrite { open, size })
    }

    /// A write whose bytes are all stored but not yet committed. Dropping
    /// it aborts the write in the background.
    pub struct StagedWrite {
        open: OpenWrite,
        size: u64,
    }

    impl StagedWrite {
        pub fn capsule_id(&self) -> CapsuleId {
            self.open
                .write
                .as_ref()
                .expect("write is open")
                .capsule_id()
        }

        /// Logical bytes written.
        pub fn size(&self) -> u64 {
            self.size
        }

        /// Commit the capsule together with `with`, all or nothing
        /// (`docs/on-disk-format.md` §5.7). Call it with the pipeline locked,
        /// so the records can be built from state no other commit changes in
        /// between. Returns the capsules `with` dropped; release each with
        /// [`RegistryPipelineHandle::release`]. If the commit fails, the
        /// write's segments are released.
        pub async fn commit(
            mut self,
            handle: &mut RegistryPipelineHandle,
            with: Vec<JournalRecord>,
        ) -> Result<Vec<Capsule>> {
            let write = self.open.take();
            handle.finish_write_with(write, with).await
        }
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
            recount_references(&storage, &registry)?;
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

    /// Ref counts aren't stored in the data log (docs/on-disk-format.md §2),
    /// so set them from the capsules the registry holds before the pipeline
    /// serves any write or delete.
    ///
    /// It also pairs the registry's journal with the data log, and keeps the
    /// segment allocator above every id the log holds, including ones from
    /// writes that never committed.
    fn recount_references(storage: &NvramBackend, registry: &crate::CapsuleRegistry) -> Result<()> {
        let log = storage.log();
        registry.bind_store(log.store_id())?;
        if let Some(max) = log.max_segment_id() {
            registry.ensure_segment_floor(max.0 + 1)?;
        }
        let counts: HashMap<SegmentId, u32> = registry.segment_refs()?;
        let report = log.recount_references(&counts);
        if let Some(first) = report.missing.first() {
            warn!(
                missing = report.missing.len(),
                first = first.0,
                "capsules reference segments the data log doesn't hold; those capsules can't be read"
            );
        }
        if !report.orphans.is_empty() {
            info!(
                orphans = report.orphans.len(),
                "the data log holds segments no capsule references; compaction reclaims them"
            );
        }
        Ok(())
    }

    fn build_nvram_encrypted_pipeline(
        storage: NvramBackend,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryEncryptedPipeline> {
        recount_references(&storage, &registry)?;
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

    fn commit(&self, records: Vec<JournalRecord>) -> Result<Vec<Capsule>> {
        self.commit(records)
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

/// Capsule records, names and the content index, kept in sled as an index of
/// the capsule journal (`docs/on-disk-format.md` §7). The journal sits next
/// to the database (`space.journal` beside `space.db`) and is the durable
/// record: losing the database costs one replay.
pub struct CapsuleRegistry {
    store: Arc<dyn MetadataStore>,
    journal: Arc<Journal>,
    /// Content hashes of segments stored by writes that haven't committed.
    /// Concurrent writes can dedup against them, but they reach the durable
    /// index only when a `CAPSULE` record that holds them is applied, so a
    /// crash can't leave an index entry for a segment compaction removes.
    pending: Arc<Mutex<HashMap<ContentHash, SegmentId>>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.db").expect(
            "failed to open registry DB at 'space.db'; check disk permissions and available space",
        )
    }

    /// Open the registry database at `path` and the capsule journal next to
    /// it. Records the index hasn't applied yet are replayed; an index that
    /// doesn't match the journal is rebuilt from it.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy().to_string();
        let store: Arc<dyn MetadataStore> = Arc::new(SledStore::open(&path_str)?);
        let journal_path = Journal::path_for(path);
        let journal = Arc::new(Journal::open(&journal_path)?);

        let apply = |record: &JournalRecord, position: JournalPosition| {
            store.apply_journal(record, &position)
        };
        match store.journal_position()? {
            Some(position) if journal.matches(&position) => journal.replay(&position, apply)?,
            Some(_) => {
                warn!(
                    journal = %journal_path.display(),
                    "the registry index doesn't match the capsule journal; rebuilding it"
                );
                store.clear_index()?;
                store.set_journal_position(&journal.start())?;
                journal.replay(&journal.start(), apply)?;
            }
            None if store.has_records()? => bail!(
                "'{path_str}' holds capsules or S3 buckets but has no capsule journal position: \
                 it was written before the capsule journal (docs/on-disk-format.md §13)"
            ),
            None => {
                store.clear_index()?;
                store.set_journal_position(&journal.start())?;
                journal.replay(&journal.start(), apply)?;
            }
        }

        Ok(Self {
            store,
            journal,
            pending: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The id of the store this registry's journal belongs to.
    pub fn store_id(&self) -> uuid::Uuid {
        self.journal.store_id()
    }

    /// Pair the journal with a data log whose store id is `store_id`. A
    /// journal with no records takes the log's id; one with records must
    /// already have it.
    pub fn bind_store(&self, store_id: uuid::Uuid) -> Result<()> {
        if self.journal.store_id() == store_id {
            return Ok(());
        }
        if !self.journal.is_empty() {
            bail!(
                "the capsule journal belongs to store {} but the data log belongs to store \
                 {store_id}; they come from different stores",
                self.journal.store_id()
            );
        }
        let position = self.journal.adopt_store_id(store_id)?;
        self.store.set_journal_position(&position)
    }

    /// How many capsule entries reference each segment, from the index.
    pub fn segment_refs(&self) -> Result<HashMap<SegmentId, u32>> {
        self.store.segment_refs()
    }

    /// Never allocate a segment id below `floor`.
    pub fn ensure_segment_floor(&self, floor: u64) -> Result<()> {
        self.store.ensure_segment_floor(floor)
    }

    /// Commit `records` all or nothing: append them to the journal, as one
    /// `BATCH` when there are several, then apply them to the index. Returns
    /// the capsules their `DROP`s removed. Nothing is written if a `CAPSULE`
    /// reuses a live id, a `DROP` names a capsule that isn't live, or a
    /// record is itself a `BATCH`.
    pub fn commit(&self, mut records: Vec<JournalRecord>) -> Result<Vec<Capsule>> {
        let mut journal = self.journal.lock();
        // The index changes only under the journal lock, so what these
        // checks see holds until the record is applied.
        let mut created: Vec<CapsuleId> = Vec::new();
        let mut segments: Vec<SegmentId> = Vec::new();
        let mut dropped: Vec<Capsule> = Vec::new();
        for record in &records {
            match record {
                JournalRecord::Capsule(manifest) => {
                    let id = manifest.capsule_id;
                    if created.contains(&id) || self.store.get_capsule(&id)?.is_some() {
                        bail!("Capsule collision (extremely unlikely)");
                    }
                    created.push(id);
                    segments.extend(manifest.entries.iter().map(|e| e.segment_id));
                }
                JournalRecord::Drop(id) => {
                    if dropped.iter().any(|c| c.id == *id) {
                        bail!("capsule {:?} is dropped twice in one commit", id);
                    }
                    let capsule = self
                        .store
                        .get_capsule(id)?
                        .ok_or_else(|| anyhow!("Capsule not found"))?;
                    dropped.push(capsule);
                }
                JournalRecord::Batch(_) => bail!("a batch can't hold another batch"),
                JournalRecord::Namespace { .. }
                | JournalRecord::Bind { .. }
                | JournalRecord::Unbind { .. } => {}
            }
        }
        let record = match records.len() {
            0 => return Ok(Vec::new()),
            1 => records.remove(0),
            _ => JournalRecord::Batch(records),
        };
        let position = journal.append(&record)?;
        self.store.apply_journal(&record, &position)?;
        drop(journal);
        if !segments.is_empty() {
            self.pending
                .lock()
                .unwrap()
                .retain(|_, seg| !segments.contains(seg));
        }
        Ok(dropped)
    }

    /// Commit a capsule from its manifest.
    pub fn create_capsule(&self, manifest: CapsuleManifest) -> Result<()> {
        self.commit(vec![JournalRecord::Capsule(Box::new(manifest))])
            .map(|_| ())
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

    /// The segment holding content with this hash: a committed one from the
    /// index, or one an open write has stored.
    pub fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        match self.store.get_content(hash) {
            Ok(Some(segment)) => Some(segment),
            _ => self.pending.lock().unwrap().get(hash).copied(),
        }
    }

    /// Offer a segment an open write just stored for other writes to dedup
    /// against. It joins the durable index when its capsule commits.
    pub fn register_content(&self, hash: ContentHash, seg_id: SegmentId) -> Result<()> {
        self.pending.lock().unwrap().entry(hash).or_insert(seg_id);
        Ok(())
    }

    /// Withdraw a segment from dedup: a pending one whose write gave up, or
    /// an indexed one no capsule references any more.
    pub fn deregister_content(&self, hash: &ContentHash, seg_id: SegmentId) -> Result<bool> {
        let mut pending = self.pending.lock().unwrap();
        if pending.get(hash) == Some(&seg_id) {
            pending.remove(hash);
            return Ok(true);
        }
        drop(pending);
        self.store.deregister_unreferenced(hash, seg_id)
    }

    pub fn list_capsules(&self, limit: usize, cursor: Option<CapsuleId>) -> Result<Vec<CapsuleId>> {
        let capsules = self.store.list_capsules(limit, cursor)?;
        Ok(capsules.into_iter().map(|c| c.id).collect())
    }

    /// Commit a delete, and return the capsule it removed. Segment bytes
    /// stay until compaction. Names that point at the capsule are left as
    /// they are; readers treat them as missing.
    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.commit(vec![JournalRecord::Drop(id)])?
            .pop()
            .ok_or_else(|| anyhow!("Capsule not found"))
    }

    /// Create namespace `ns` if it doesn't exist.
    pub fn create_namespace(&self, ns: &str) -> Result<()> {
        if self.store.get_namespace(ns)?.is_some() {
            return Ok(());
        }
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        self.commit(vec![JournalRecord::Namespace {
            ns: ns.to_string(),
            created_at,
        }])
        .map(|_| ())
    }

    /// When namespace `ns` was created, or `None` if it doesn't exist.
    pub fn namespace_created_at(&self, ns: &str) -> Result<Option<u64>> {
        self.store.get_namespace(ns)
    }

    /// The capsule `name` in `ns` points at.
    pub fn name(&self, ns: &str, name: &str) -> Result<Option<CapsuleId>> {
        self.store.get_name(ns, name)
    }

    /// Names in `ns` that start with `prefix` and sort after `start_after`,
    /// in byte order, at most `limit` of them.
    pub fn list_names(
        &self,
        ns: &str,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, CapsuleId)>> {
        self.store.list_names(ns, prefix, start_after, limit)
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
            journal: Arc::clone(&self.journal),
            pending: Arc::clone(&self.pending),
        }
    }
}

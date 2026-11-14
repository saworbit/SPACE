use anyhow::{anyhow, bail, Result};

use common::format::{CapsuleManifest, JournalRecord, StoreKey};
use common::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use crate::journal::{Journal, JournalPosition, JournalUsage};
use crate::store::{MetadataStore, SledStore};

mod journal;
mod store;

pub use journal::JournalCompaction;
pub use store::Binding;

const DEFAULT_PAGE_SIZE: usize = 1024;

pub mod dedup; // NEW
pub mod error;
pub mod pipeline;

pub mod scrub_executor;

pub use error::{CompressionError, DedupError, PipelineError};

pub mod modular_pipeline {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;
    use common::format::{JournalRecord, StoreKey};
    use common::scrub::{ScrubConfig, ScrubKind, ScrubReport, ScrubState};
    use common::traits::StorageBackend;
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
    use tokio::sync::{watch, Mutex as TokioMutex};
    use tokio::task::JoinHandle;
    use tracing::{info, warn};

    use crate::scrub_executor::ScrubExecutor;

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

        /// The NVRAM data log the pipeline stores segments in, or `None` for
        /// a file backend.
        pub fn data_log(&self) -> Option<NvramLog> {
            match self {
                Self::Encrypted(p) => Some(p.storage().log().clone()),
                Self::Plain(p) => Some(p.storage().log().clone()),
                Self::FsEncrypted(_) | Self::FsPlain(_) => None,
            }
        }

        fn key_manager(&self) -> Option<Arc<Mutex<KeyManager>>> {
            match self {
                Self::Encrypted(p) => p.keyring().map(KeyManagerKeyring::manager),
                Self::FsEncrypted(p) => p.keyring().map(KeyManagerKeyring::manager),
                Self::Plain(_) | Self::FsPlain(_) => None,
            }
        }

        /// The key this pipeline encrypts with, as the store records it.
        /// `None` without a key.
        pub fn store_key(&self) -> Option<StoreKey> {
            self.key_manager().map(|manager| store_key_of(&manager))
        }

        /// Fails when this pipeline refuses writes: it was opened without
        /// the key of an encrypted store (#157).
        pub fn check_writable(&self) -> Result<()> {
            self.registry().check_writable()
        }

        /// Scrub the store in the background on `config`'s schedule
        /// ([`ScrubExecutor::spawn`]). The executor gets the pipeline's key,
        /// so deep scrubs check encrypted segments' MACs, and keeps its
        /// schedule in `space.db`, so a restart scrubs only segments that are
        /// due (#134). It reads segments straight from the backend, without
        /// the pipeline lock.
        pub fn spawn_scrub(
            &self,
            config: ScrubConfig,
        ) -> (JoinHandle<()>, watch::Receiver<ScrubState>) {
            let key = self.key_manager();
            let schedule = self.registry().scrub_schedule();
            match self {
                Self::Encrypted(p) => executor(p.storage().clone(), key, schedule).spawn(config),
                Self::Plain(p) => executor(p.storage().clone(), key, schedule).spawn(config),
                Self::FsEncrypted(p) => executor(p.storage().clone(), key, schedule).spawn(config),
                Self::FsPlain(p) => executor(p.storage().clone(), key, schedule).spawn(config),
            }
        }

        /// Scrub every segment of the store once, now, as `spacectl scrub`
        /// does: no per-cycle cap and no throttling. The results go into the
        /// schedule in `space.db`, so `serve-s3` doesn't check the same
        /// segments again before they're due.
        pub async fn scrub_all(&self, kind: ScrubKind) -> ScrubReport {
            let config = ScrubConfig {
                // Every segment is due, whatever the schedule says.
                light_interval: Duration::ZERO,
                deep_interval: Duration::ZERO,
                max_segments_per_cycle: usize::MAX,
                inter_segment_delay: Duration::ZERO,
                max_bytes_per_sec: None,
            };
            let key = self.key_manager();
            let schedule = self.registry().scrub_schedule();
            match self {
                Self::Encrypted(p) => {
                    executor(p.storage().clone(), key, schedule)
                        .scrub_cycle(&config, kind)
                        .await
                }
                Self::Plain(p) => {
                    executor(p.storage().clone(), key, schedule)
                        .scrub_cycle(&config, kind)
                        .await
                }
                Self::FsEncrypted(p) => {
                    executor(p.storage().clone(), key, schedule)
                        .scrub_cycle(&config, kind)
                        .await
                }
                Self::FsPlain(p) => {
                    executor(p.storage().clone(), key, schedule)
                        .scrub_cycle(&config, kind)
                        .await
                }
            }
        }
    }

    fn executor<B: StorageBackend>(
        backend: B,
        key: Option<Arc<Mutex<KeyManager>>>,
        schedule: Arc<dyn common::scrub::ScheduleStore>,
    ) -> ScrubExecutor<B> {
        let executor = match key {
            Some(key) => ScrubExecutor::with_key_manager(backend, key),
            None => ScrubExecutor::new(backend),
        };
        executor.with_schedule_store(schedule)
    }

    /// What a [`reclaim`] pass did.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct ReclaimReport {
        /// Segments garbage collection removed (file backends).
        pub collected: usize,
        /// Bytes compaction reclaimed, when it ran (the NVRAM log).
        pub compacted: Option<u64>,
        /// Bytes journal compaction reclaimed, when it ran.
        pub journal_compacted: Option<u64>,
    }

    /// Whether a scheduled pass should compact `log`: once a quarter of it,
    /// and at least one default segment, is reclaimable. Compaction copies
    /// every live record, so it waits until there's enough to gain.
    pub fn worth_compacting(log: &NvramLog) -> bool {
        let reclaimable = log.reclaimable_bytes();
        reclaimable >= common::SEGMENT_SIZE as u64 && reclaimable.saturating_mul(4) >= log.size()
    }

    /// One reclaim pass, as `serve-s3` schedules them.
    ///
    /// On the NVRAM log it compacts, if [`worth_compacting`] says so. A
    /// segment no capsule references has no references in the log's counts,
    /// so compaction leaves it out and garbage collection has nothing to
    /// add. Compaction runs on a blocking thread without the pipeline lock,
    /// so writes and reads carry on.
    ///
    /// On a file backend it garbage-collects, with the pipeline locked: each
    /// unreferenced segment is a file to delete.
    ///
    /// Then, on either, it compacts the capsule journal if
    /// [`CapsuleRegistry::compact_journal_if_worthwhile`](crate::CapsuleRegistry::compact_journal_if_worthwhile)
    /// finds enough of it dead, also on a blocking thread without the
    /// pipeline lock.
    pub async fn reclaim(
        pipeline: &Arc<TokioMutex<RegistryPipelineHandle>>,
    ) -> Result<ReclaimReport> {
        let mut report = ReclaimReport::default();
        let (log, registry) = {
            let mut handle = pipeline.lock().await;
            let log = handle.data_log();
            if log.is_none() {
                report.collected = handle.garbage_collect().await?;
            }
            (log, handle.registry().clone())
        };
        if let Some(log) = log.filter(worth_compacting) {
            report.compacted = Some(tokio::task::spawn_blocking(move || log.compact()).await??);
        }
        report.journal_compacted =
            tokio::task::spawn_blocking(move || registry.compact_journal_if_worthwhile())
                .await??
                .map(|done| done.reclaimed());
        Ok(report)
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
        mut registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        // A set but invalid SPACE_MASTER_KEY is an error, not a quiet
        // fallback to the plain pipeline (#84).
        if let Some(manager) = KeyManager::from_env_if_set()? {
            let km = Arc::new(Mutex::new(manager));
            let pipeline = build_nvram_encrypted_pipeline(storage, registry, km)?;
            Ok(RegistryPipelineHandle::Encrypted(pipeline))
        } else {
            recount_references(&storage, &mut registry, None, plaintext_allowed()?)?;
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
        mut registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        // A set but invalid SPACE_MASTER_KEY is an error, not a quiet
        // fallback to the plain pipeline (#84).
        if let Some(manager) = KeyManager::from_env_if_set()? {
            let km = Arc::new(Mutex::new(manager));
            let pipeline = build_fs_encrypted_pipeline(storage, registry, km)?;
            Ok(RegistryPipelineHandle::FsEncrypted(pipeline))
        } else {
            recount_fs_references(&storage, &mut registry, None, plaintext_allowed()?)?;
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

    /// The store's record of `key_manager`'s key.
    fn store_key_of(key_manager: &Mutex<KeyManager>) -> StoreKey {
        let manager = key_manager.lock().unwrap_or_else(|e| e.into_inner());
        StoreKey {
            key_version: manager.current_version(),
            fingerprint: manager.fingerprint(),
        }
    }

    /// Whether `SPACE_ALLOW_PLAINTEXT=1` lets an encrypted store open
    /// without its key, storing new objects unencrypted (#157).
    fn plaintext_allowed() -> Result<bool> {
        match std::env::var_os("SPACE_ALLOW_PLAINTEXT") {
            None => Ok(false),
            Some(value) if value == "1" => Ok(true),
            Some(value) => anyhow::bail!("SPACE_ALLOW_PLAINTEXT must be 1 or unset, not {value:?}"),
        }
    }

    /// Ref counts aren't stored in the data log (docs/on-disk-format.md §2),
    /// so set them from the capsules the registry holds before the pipeline
    /// serves any write or delete.
    ///
    /// It also pairs the registry's journal with the data log, checks `key`
    /// against the store's ([`crate::CapsuleRegistry::check_store_key`]),
    /// and keeps the segment allocator above every id the log has held,
    /// including ones from writes that never committed and ones compaction
    /// removed.
    fn recount_references(
        storage: &NvramBackend,
        registry: &mut crate::CapsuleRegistry,
        key: Option<StoreKey>,
        allow_plaintext: bool,
    ) -> Result<()> {
        let log = storage.log();
        registry.bind_store(log.store_id(), !log.list_segment_ids().is_empty())?;
        registry.check_store_key(key, allow_plaintext)?;
        registry.ensure_segment_floor(log.segment_id_floor())?;
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

    /// [`recount_references`] for a storage root: ref counts aren't stored
    /// in its segment files either (docs/on-disk-format.md §10). The journal
    /// is paired with the root's store id, `key` is checked, and the
    /// allocator is kept above every segment file, so a lost `space.db`
    /// can't lead to an id that overwrites one.
    fn recount_fs_references(
        storage: &AutoFsBackend,
        registry: &mut crate::CapsuleRegistry,
        key: Option<StoreKey>,
        allow_plaintext: bool,
    ) -> Result<()> {
        let floor = storage.segment_id_floor()?;
        registry.bind_store(storage.store_id(), floor > 0)?;
        registry.check_store_key(key, allow_plaintext)?;
        registry.ensure_segment_floor(floor)?;
        let counts: HashMap<SegmentId, u32> = registry.segment_refs()?;
        let report = storage.recount_references(&counts)?;
        if let Some(first) = report.missing.first() {
            warn!(
                missing = report.missing.len(),
                first = first.0,
                "capsules reference segments the storage root doesn't hold; those capsules can't be read"
            );
        }
        if !report.orphans.is_empty() {
            info!(
                orphans = report.orphans.len(),
                "the storage root holds segments no capsule references; garbage collection deletes them"
            );
        }
        Ok(())
    }

    fn build_nvram_encrypted_pipeline(
        storage: NvramBackend,
        mut registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryEncryptedPipeline> {
        let key = Some(store_key_of(&key_manager));
        recount_references(&storage, &mut registry, key, false)?;
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
        mut registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryFsEncryptedPipeline> {
        let key = Some(store_key_of(&key_manager));
        recount_fs_references(&storage, &mut registry, key, false)?;
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

    /// From the index's reference counts, without loading every capsule.
    fn referenced_segments(&self) -> Result<std::collections::HashSet<SegmentId>> {
        Ok(self.segment_refs()?.into_keys().collect())
    }
}

/// A scheduled pass compacts the capsule journal once at least this many
/// bytes of it, and a quarter of it, are dead.
const JOURNAL_MIN_RECLAIM: u64 = 1 << 20;

fn journal_worth_compacting(size: u64, reclaimable: u64) -> bool {
    reclaimable >= JOURNAL_MIN_RECLAIM && reclaimable.saturating_mul(4) >= size
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
    /// The store's key, when a pipeline opened this registry without it:
    /// writes are then refused (#157). Each clone has its own.
    writes_need: Option<StoreKey>,
}

/// The registry's scrub schedule, as a [`common::scrub::ScheduleStore`].
struct RegistryScrubSchedule(Arc<dyn MetadataStore>);

impl common::scrub::ScheduleStore for RegistryScrubSchedule {
    fn load(&self) -> Result<Vec<(SegmentId, common::scrub::ScrubTimes)>> {
        self.0.scrub_times()
    }

    fn save(&self, changes: &[(SegmentId, Option<common::scrub::ScrubTimes>)]) -> Result<()> {
        self.0.put_scrub_times(changes)
    }
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
    ///
    /// The journal is the record of what the store holds, so a database
    /// that has been used is never opened without it (#194). A new journal
    /// would rebuild the index as empty, and the next reclaim would then
    /// delete every segment.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy().to_string();
        let store: Arc<dyn MetadataStore> = Arc::new(SledStore::open(&path_str)?);
        let journal_path = Journal::path_for(path);
        if !journal_path.exists() && store.journal_position()?.is_some() {
            bail!(
                "the capsule journal '{}' is missing, but '{path_str}' shows this store has \
                 been used. The journal records every capsule, bucket and key, so the store \
                 won't open without it, and nothing has been changed. Restore the journal \
                 from the same copy as the rest of the store.",
                journal_path.display()
            );
        }
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
            writes_need: None,
        })
    }

    /// The id of the store this registry's journal belongs to.
    pub fn store_id(&self) -> uuid::Uuid {
        self.journal.store_id()
    }

    /// Whether the capsule journal has never held a record. Until it has,
    /// it can still be paired with any store's data.
    pub fn journal_is_empty(&self) -> bool {
        self.journal.is_empty()
    }

    /// Pair the journal with a data log or storage root whose store id is
    /// `store_id`, and which holds segments if `holds_segments`. A journal
    /// with no records takes a new store's id; one with records must
    /// already have it. A journal with no records is refused for a store
    /// that already holds segments (#194): the journal that records them
    /// is missing, and with this one every segment would look unreferenced,
    /// so the next reclaim would delete it.
    pub fn bind_store(&self, store_id: uuid::Uuid, holds_segments: bool) -> Result<()> {
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
        if holds_segments {
            bail!(
                "the capsule journal '{}' is new, but the store's segments already hold data \
                 for store {store_id}. The journal that records them is missing, and without \
                 it the next reclaim would delete every segment, so the store won't open. \
                 Restore the journal from the same copy as the segments.",
                self.journal.path().display()
            );
        }
        let position = self.journal.adopt_store_id(store_id)?;
        self.store.set_journal_position(&position)
    }

    /// The key the store was first opened with, from its journal. `None`
    /// for a store that has never been opened with one.
    pub fn store_key(&self) -> Option<StoreKey> {
        self.journal.store_key()
    }

    /// Check the key a pipeline opens with, `key`, against the one the
    /// journal records (#157). A store's first open with a key records it.
    /// From then on another key is refused, and without one this registry
    /// refuses writes, unless `allow_plaintext` is set: new objects are
    /// stored unencrypted only when someone asked for that. Reads, deletes
    /// and maintenance need no key.
    pub fn check_store_key(&mut self, key: Option<StoreKey>, allow_plaintext: bool) -> Result<()> {
        self.writes_need = None;
        let mut journal = self.journal.lock();
        match (journal.store_key(), key) {
            (None, None) => Ok(()),
            (None, Some(key)) => {
                let record = JournalRecord::Key(key);
                let position = journal.append(&record)?;
                self.store.apply_journal(&record, &position)?;
                info!(
                    key = %key.fingerprint_hex(),
                    "this store is now encrypted: from now on it refuses another key, and \
                     writes without this one"
                );
                Ok(())
            }
            (Some(stored), Some(key)) if stored.fingerprint == key.fingerprint => Ok(()),
            (Some(stored), Some(key)) => bail!(
                "SPACE_MASTER_KEY isn't this store's key: the store's key has fingerprint {}, \
                 and this one has {}",
                stored.fingerprint_hex(),
                key.fingerprint_hex()
            ),
            (Some(stored), None) if allow_plaintext => {
                warn!(
                    key = %stored.fingerprint_hex(),
                    "this store is encrypted, but SPACE_ALLOW_PLAINTEXT=1 is set and \
                     SPACE_MASTER_KEY isn't: new objects are stored unencrypted, and encrypted \
                     ones can't be read"
                );
                Ok(())
            }
            (Some(stored), None) => {
                info!(
                    key = %stored.fingerprint_hex(),
                    "this store is encrypted and SPACE_MASTER_KEY isn't set, so writes are refused"
                );
                self.writes_need = Some(stored);
                Ok(())
            }
        }
    }

    /// Fails, naming SPACE_MASTER_KEY, when this registry's pipeline was
    /// opened without the key of an encrypted store (#157).
    pub fn check_writable(&self) -> Result<()> {
        match self.writes_need {
            None => Ok(()),
            Some(stored) => bail!(
                "this store is encrypted: set SPACE_MASTER_KEY to its key (fingerprint {}) to \
                 write to it. To store new objects unencrypted anyway, set \
                 SPACE_ALLOW_PLAINTEXT=1",
                stored.fingerprint_hex()
            ),
        }
    }

    /// How many capsule entries reference each segment, from the index.
    pub fn segment_refs(&self) -> Result<HashMap<SegmentId, u32>> {
        self.store.segment_refs()
    }

    /// Compact the capsule journal now if anything in it is dead, as
    /// `spacectl compact` does: write its live capsules, namespaces and
    /// names to the next generation and rename that over it
    /// (`docs/on-disk-format.md` §9.2). Commits wait only while the records
    /// committed meanwhile are copied and the file is swapped. Returns
    /// `None` if there was nothing to reclaim.
    pub fn compact_journal(&self) -> Result<Option<JournalCompaction>> {
        self.compact_journal_with(|usage| usage.reclaimable() > 0, |_| false)
    }

    /// Compact the capsule journal once a quarter of it, and at least 1 MiB,
    /// is dead, as `serve-s3`'s reclaim pass does. Finding out reads the
    /// whole journal, so a pass does it only the first time since the store
    /// was opened, and then when an estimate the journal keeps as records
    /// are appended says compaction may be worth it.
    pub fn compact_journal_if_worthwhile(&self) -> Result<Option<JournalCompaction>> {
        let (size, dead) = self.journal.reclaim_estimate();
        let due = match dead {
            None => size >= JOURNAL_MIN_RECLAIM,
            Some(dead) => journal_worth_compacting(size, dead),
        };
        if !due {
            return Ok(None);
        }
        self.compact_journal_with(
            |usage| journal_worth_compacting(usage.size, usage.reclaimable()),
            |_| false,
        )
    }

    /// [`CapsuleRegistry::compact_journal`], with the decision and the
    /// points to stop at left to the caller. The index records the new
    /// generation under the journal lock, so no commit lands in between.
    fn compact_journal_with(
        &self,
        worth: impl FnOnce(&JournalUsage) -> bool,
        stop_at: impl FnMut(journal::Step) -> bool,
    ) -> Result<Option<JournalCompaction>> {
        let store = Arc::clone(&self.store);
        self.journal.compact_with(
            worth,
            move |position| store.set_journal_position(&position),
            stop_at,
        )
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
                    // A write of segments it found by dedup allocates none.
                    self.check_writable()?;
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
                JournalRecord::Key(_) => {
                    bail!("the store's key is recorded by check_store_key")
                }
                JournalRecord::Namespace { .. }
                | JournalRecord::DropNamespace { .. }
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
        // Every new segment starts here, so nothing reaches the store.
        self.check_writable()?;
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

    /// Namespaces that start with `prefix` and sort after `start_after`,
    /// in byte order, at most `limit` of them, with their creation times.
    pub fn list_namespaces(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        self.store.list_namespaces(prefix, start_after, limit)
    }

    /// The capsule `name` in `ns` points at.
    pub fn name(&self, ns: &str, name: &str) -> Result<Option<CapsuleId>> {
        Ok(self.store.get_name(ns, name)?.map(|b| b.capsule_id))
    }

    /// The capsule `name` in `ns` points at, and the metadata kept with it.
    pub fn binding(&self, ns: &str, name: &str) -> Result<Option<Binding>> {
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
    ) -> Result<Vec<(String, Binding)>> {
        self.store.list_names(ns, prefix, start_after, limit)
    }

    /// The scrub schedule kept in `space.db`, for
    /// [`ScrubExecutor::with_schedule_store`](crate::scrub_executor::ScrubExecutor::with_schedule_store).
    pub fn scrub_schedule(&self) -> Arc<dyn common::scrub::ScheduleStore> {
        Arc::new(RegistryScrubSchedule(Arc::clone(&self.store)))
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
            writes_need: self.writes_need,
        }
    }
}

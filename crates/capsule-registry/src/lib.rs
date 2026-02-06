use anyhow::{anyhow, Result};
#[cfg(feature = "advanced-security")]
use common::security::bloom_dedup::BloomFilterWrapper;
#[cfg(feature = "advanced-security")]
use common::security::DedupOptimizer;
use common::*;
use encryption::keymanager::MASTER_KEY_SIZE;
use encryption::KeyManager;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::consensus::RaftNode;
use crate::metadata_ops::{MetadataOp, OpResult};
use crate::store::{MetadataStore, SledStore};

mod consensus;
pub mod mesh;
mod metadata_ops;
mod raft_rpc;
mod store;
#[cfg(feature = "podms")]
mod transform;

const DEFAULT_PAGE_SIZE: usize = 1024;

pub mod dedup; // NEW
pub mod error;
pub mod gc;
pub mod pipeline;
#[cfg(feature = "podms")]
pub mod runtime;

pub use error::{CompressionError, DedupError, PipelineError};
#[cfg(feature = "podms")]
pub use runtime::RuntimeHandles;
#[cfg(feature = "podms")]
pub use transform::RegistryTransformOps;

#[cfg(feature = "modular_pipeline")]
pub mod modular_pipeline {
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use common::{traits::DataStream, CapsuleId, Policy};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    pub use pipeline::{
        pipeline_with_nvram, pipeline_with_nvram_xts, DefaultPipeline, DefaultPolicyEvaluator,
        InMemoryPipeline, KeyManagerKeyring, NoopEncryptor, NullKeyring, NvramPipeline,
        NvramPipelineWithEncryption, Pipeline, PipelineBuilder, XtsEncryptor,
    };
    pub use storage::{AutoFsBackend, InMemoryBackend, NvramBackend};

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
        pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
            match self {
                Self::Encrypted(p) => p.write_capsule(data, policy).await,
                Self::Plain(p) => p.write_capsule(data, policy).await,
                Self::FsEncrypted(p) => p.write_capsule(data, policy).await,
                Self::FsPlain(p) => p.write_capsule(data, policy).await,
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
        let reheat_on_read = std::env::var("SPACE_REHEAT_ON_READ")
            .ok()
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let cold_root = std::env::var("SPACE_COLD_ROOT")
            .ok()
            .map(std::path::PathBuf::from);

        let storage = AutoFsBackend::open(root).await?;
        let storage = if let Some(cold) = cold_root {
            storage.with_tiering(cold, reheat_on_read)?
        } else {
            storage
        };

        registry_pipeline_from_fs_backend(storage, registry)
    }

    fn registry_pipeline_from_nvram_backend(
        storage: NvramBackend,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        if let Ok(manager) = KeyManager::from_env() {
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
        if let Ok(manager) = KeyManager::from_env() {
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
    raft: RaftNode,
    #[cfg(feature = "advanced-security")]
    bloom_filter: Option<Arc<BloomFilterWrapper>>,
    key_manager: Arc<Mutex<KeyManager>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.db").expect("failed to open registry DB at 'space.db'; check disk permissions and available space")
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let store: Arc<dyn MetadataStore> = Arc::new(SledStore::open(&path_str)?);
        let raft = RaftNode::new(Arc::clone(&store));

        let key_manager =
            KeyManager::from_env().unwrap_or_else(|_| KeyManager::new([0u8; MASTER_KEY_SIZE]));
        let key_manager = Arc::new(Mutex::new(key_manager));

        #[cfg(feature = "advanced-security")]
        let bloom_filter = Self::configure_bloom(&*store)?;

        Ok(Self {
            store,
            raft,
            #[cfg(feature = "advanced-security")]
            bloom_filter,
            key_manager,
        })
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
        match self.raft.propose(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
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
        match self.raft.propose(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(true),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
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
        match self.raft.propose(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
        }
    }

    pub fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        #[cfg(feature = "advanced-security")]
        if let Some(filter) = &self.bloom_filter {
            if !filter.might_contain(hash) {
                return None;
            }
        }
        self.store.get_content(hash).ok().flatten()
    }

    pub fn register_content(&self, hash: ContentHash, seg_id: SegmentId) -> Result<()> {
        match self.raft.propose(MetadataOp::RegisterContent {
            hash: hash.clone(),
            segment: seg_id,
        })? {
            OpResult::Ok => {
                #[cfg(feature = "advanced-security")]
                if let Some(filter) = &self.bloom_filter {
                    filter.record_insertion(&hash);
                }
                Ok(())
            }
            other => anyhow::bail!("unexpected raft response: {:?}", other),
        }
    }

    pub fn deregister_content(&self, hash: &ContentHash, seg_id: SegmentId) -> Result<bool> {
        match self.raft.propose(MetadataOp::DeregisterContent {
            hash: hash.clone(),
            segment: seg_id,
        })? {
            OpResult::Ok => {
                #[cfg(feature = "advanced-security")]
                if let Some(filter) = &self.bloom_filter {
                    filter.record_removal(hash);
                }
                Ok(true)
            }
            OpResult::NotFound => Ok(false),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
        }
    }

    pub fn add_deduped_bytes(&self, capsule_id: CapsuleId, bytes: u64) -> Result<()> {
        let mut capsule = match self.lookup(capsule_id) {
            Ok(capsule) => capsule,
            Err(_) => return Ok(()), // Capsule deleted before dedup update; treat as noop
        };
        capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
        match self.raft.propose(MetadataOp::PutCapsule(capsule))? {
            OpResult::Ok => Ok(()),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
        }
    }

    pub fn list_capsules(&self, limit: usize, cursor: Option<CapsuleId>) -> Result<Vec<CapsuleId>> {
        let capsules = self.store.list_capsules(limit, cursor)?;
        Ok(capsules.into_iter().map(|c| c.id).collect())
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        match self.raft.propose(MetadataOp::DeleteCapsule(id))? {
            OpResult::CapsuleFound(capsule) => Ok(capsule),
            OpResult::NotFound => Err(anyhow!("Capsule not found")),
            other => anyhow::bail!("unexpected raft response: {:?}", other),
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

    pub fn key_manager(&self) -> &Arc<Mutex<KeyManager>> {
        &self.key_manager
    }

    #[cfg(feature = "advanced-security")]
    fn configure_bloom(store: &dyn MetadataStore) -> Result<Option<Arc<BloomFilterWrapper>>> {
        let capacity = std::env::var("SPACE_BLOOM_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10_000_000);
        let fp_rate = std::env::var("SPACE_BLOOM_FPR")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.001);

        let hashes = store
            .list_content()
            .unwrap_or_default()
            .into_iter()
            .map(|(hash, _)| hash)
            .collect::<Vec<_>>();

        let filter = if hashes.is_empty() {
            BloomFilterWrapper::new(capacity, fp_rate)
        } else {
            BloomFilterWrapper::with_existing(capacity, fp_rate, hashes)
        };

        Ok(Some(Arc::new(filter)))
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
            raft: self.raft.clone(),
            #[cfg(feature = "advanced-security")]
            bloom_filter: self.bloom_filter.clone(),
            key_manager: Arc::clone(&self.key_manager),
        }
    }
}

// Implement ContentStore trait for mesh replication (PODMS feature only)
#[cfg(feature = "podms")]
impl scaling::ContentStore for CapsuleRegistry {
    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.lookup_content(hash)
    }

    fn register_content(&self, hash: &ContentHash, segment_id: SegmentId) {
        let _ = self.register_content(hash.clone(), segment_id);
    }
}

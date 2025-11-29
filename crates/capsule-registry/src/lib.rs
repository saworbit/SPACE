use anyhow::{anyhow, Result};
#[cfg(feature = "advanced-security")]
use common::security::bloom_dedup::BloomFilterWrapper;
#[cfg(feature = "advanced-security")]
use common::security::DedupOptimizer;
use common::*;
use std::path::Path;
use std::sync::Arc;

use crate::store::{MetadataStore, SledStore};

mod consensus;
mod store;

pub mod dedup; // NEW
pub mod error;
pub mod gc;
pub mod pipeline;
#[cfg(feature = "podms")]
pub mod runtime;

pub use error::{CompressionError, DedupError, PipelineError};
#[cfg(feature = "podms")]
pub use runtime::RuntimeHandles;

#[cfg(feature = "modular_pipeline")]
pub mod modular_pipeline {
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use common::{CapsuleId, Policy};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    pub use pipeline::{
        pipeline_with_nvram, pipeline_with_nvram_xts, DefaultPipeline, DefaultPolicyEvaluator,
        InMemoryPipeline, KeyManagerKeyring, NoopEncryptor, NullKeyring, NvramPipeline,
        NvramPipelineWithEncryption, Pipeline, PipelineBuilder, XtsEncryptor,
    };
    pub use storage::{InMemoryBackend, NvramBackend};

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

    pub enum RegistryPipelineHandle {
        Encrypted(RegistryEncryptedPipeline),
        Plain(RegistryPlainPipeline),
    }

    impl RegistryPipelineHandle {
        pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
            match self {
                Self::Encrypted(p) => p.write_capsule(data, policy).await,
                Self::Plain(p) => p.write_capsule(data, policy).await,
            }
        }

        pub async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
            match self {
                Self::Encrypted(p) => p.read_capsule(id).await,
                Self::Plain(p) => p.read_capsule(id).await,
            }
        }

        pub async fn delete_capsule(&mut self, id: CapsuleId) -> Result<()> {
            match self {
                Self::Encrypted(p) => p.delete_capsule(id).await,
                Self::Plain(p) => p.delete_capsule(id).await,
            }
        }

        pub async fn garbage_collect(&mut self) -> Result<usize> {
            match self {
                Self::Encrypted(p) => p.garbage_collect().await,
                Self::Plain(p) => p.garbage_collect().await,
            }
        }
    }

    pub fn registry_pipeline_from_env<P: AsRef<std::path::Path>>(
        path: P,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        let backend = NvramBackend::open(path)?;
        registry_pipeline_from_backend(backend, registry)
    }

    pub fn registry_pipeline_from_log(
        log: NvramLog,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        let backend = NvramBackend::from_log(log);
        registry_pipeline_from_backend(backend, registry)
    }

    pub fn registry_nvram_pipeline_with_encryption<P: AsRef<std::path::Path>>(
        path: P,
        registry: crate::CapsuleRegistry,
        key_manager: Arc<Mutex<KeyManager>>,
    ) -> Result<RegistryEncryptedPipeline> {
        let storage = NvramBackend::open(path)?;
        build_encrypted_pipeline(storage, registry, key_manager)
    }

    fn registry_pipeline_from_backend(
        storage: NvramBackend,
        registry: crate::CapsuleRegistry,
    ) -> Result<RegistryPipelineHandle> {
        if let Ok(manager) = KeyManager::from_env() {
            let km = Arc::new(Mutex::new(manager));
            let pipeline = build_encrypted_pipeline(storage, registry, km)?;
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

    fn build_encrypted_pipeline(
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
    #[cfg(feature = "advanced-security")]
    bloom_filter: Option<Arc<BloomFilterWrapper>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.db").expect("Failed to open registry DB")
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let store = Arc::new(SledStore::open(&path_str)?);

        #[cfg(feature = "advanced-security")]
        let bloom_filter = Self::configure_bloom(&*store)?;

        Ok(Self {
            store,
            #[cfg(feature = "advanced-security")]
            bloom_filter,
        })
    }

    fn insert_capsule(&self, capsule: Capsule) -> Result<()> {
        if self.store.get_capsule(&capsule.id)?.is_some() {
            anyhow::bail!("Capsule collision (extremely unlikely)");
        }
        self.store.put_capsule(&capsule)
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
        self.store.put_capsule(&capsule)
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
        self.store.put_content(&hash, seg_id)?;
        #[cfg(feature = "advanced-security")]
        if let Some(filter) = &self.bloom_filter {
            filter.record_insertion(&hash);
        }
        Ok(())
    }

    pub fn deregister_content(&self, hash: &ContentHash, seg_id: SegmentId) -> Result<bool> {
        if let Some(current) = self.store.get_content(hash)? {
            if current == seg_id {
                self.store.delete_content(hash)?;
                #[cfg(feature = "advanced-security")]
                if let Some(filter) = &self.bloom_filter {
                    filter.record_removal(hash);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn add_deduped_bytes(&self, capsule_id: CapsuleId, bytes: u64) -> Result<()> {
        self.store.add_deduped_bytes(&capsule_id, bytes)
    }

    pub fn list_capsules(&self) -> Vec<CapsuleId> {
        self.store
            .list_capsules()
            .map(|capsules| capsules.into_iter().map(|c| c.id).collect())
            .unwrap_or_default()
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.store
            .delete_capsule(&id)?
            .ok_or_else(|| anyhow!("Capsule not found"))
    }

    pub fn get_dedup_stats(&self) -> (usize, usize) {
        let content_store = self.store.list_content().unwrap_or_default();
        let capsules = self.store.list_capsules().unwrap_or_default();

        let total_segments: usize = capsules.iter().map(|c| c.segments.len()).sum();
        let unique_segments = content_store.len();

        (total_segments, unique_segments)
    }

    pub fn capsules(&self) -> Vec<Capsule> {
        self.store.list_capsules().unwrap_or_default()
    }

    pub fn content_entries(&self) -> Vec<(ContentHash, SegmentId)> {
        self.store.list_content().unwrap_or_default()
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
            #[cfg(feature = "advanced-security")]
            bloom_filter: self.bloom_filter.clone(),
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

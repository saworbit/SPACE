use anyhow::Result;
#[cfg(feature = "advanced-security")]
use common::security::bloom_dedup::BloomFilterWrapper;
#[cfg(feature = "advanced-security")]
use common::security::DedupOptimizer;
use common::Policy;
use common::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, RwLock};

pub mod dedup; // NEW
pub mod error;
pub mod gc;
pub mod pipeline;

pub use error::{CompressionError, DedupError, PipelineError};

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
        Ok(CapsuleRegistry::alloc_segment(self))
    }

    fn lookup_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        CapsuleRegistry::lookup(self, id)
    }

    fn create_capsule(
        &self,
        id: CapsuleId,
        size: u64,
        policy: &Policy,
        segments: Vec<SegmentId>,
        stats: &common::traits::DedupStats,
    ) -> Result<()> {
        CapsuleRegistry::create_capsule_with_segments(self, id, size, segments, policy.clone())?;
        let mut capsules = self.capsules.write().unwrap();
        if let Some(capsule) = capsules.get_mut(&id) {
            capsule.policy = policy.clone();
            capsule.deduped_bytes = stats.bytes_saved;
        }
        drop(capsules);
        CapsuleRegistry::save(self)?;
        Ok(())
    }

    fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        CapsuleRegistry::delete_capsule(self, id)
    }

    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        CapsuleRegistry::lookup_content(self, hash)
    }

    fn register_content(&self, hash: ContentHash, segment: SegmentId) -> Result<()> {
        CapsuleRegistry::register_content(self, hash, segment)
    }

    fn deregister_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        CapsuleRegistry::deregister_content(self, hash, segment)
    }

    fn capsules(&self) -> Vec<Capsule> {
        self.capsules.read().unwrap().values().cloned().collect()
    }

    fn content_entries(&self) -> Vec<(ContentHash, SegmentId)> {
        self.content_store
            .read()
            .unwrap()
            .iter()
            .map(|(hash, seg)| (hash.clone(), *seg))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryState {
    capsules: HashMap<CapsuleId, Capsule>,
    next_segment_id: u64,
    // Phase 2.2: Content-addressed storage for deduplication
    #[serde(default)]
    content_store: HashMap<ContentHash, SegmentId>,
}

pub struct CapsuleRegistry {
    capsules: Arc<RwLock<HashMap<CapsuleId, Capsule>>>,
    next_segment_id: Arc<RwLock<u64>>,
    metadata_path: String,
    // Phase 2.2: Content store for deduplication
    content_store: Arc<RwLock<HashMap<ContentHash, SegmentId>>>,
    #[cfg(feature = "advanced-security")]
    bloom_filter: Option<Arc<BloomFilterWrapper>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.metadata").expect("Failed to open registry")
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let metadata_path = path.as_ref().to_string_lossy().to_string();

        // Try to load existing state
        let (capsules, next_segment_id, content_store) = if Path::new(&metadata_path).exists() {
            let data = fs::read_to_string(&metadata_path)?;
            let state: RegistryState = serde_json::from_str(&data)?;
            (state.capsules, state.next_segment_id, state.content_store)
        } else {
            (HashMap::new(), 0, HashMap::new())
        };

        #[cfg(feature = "advanced-security")]
        let bloom_filter = Self::configure_bloom(Some(&content_store));

        Ok(Self {
            capsules: Arc::new(RwLock::new(capsules)),
            next_segment_id: Arc::new(RwLock::new(next_segment_id)),
            metadata_path,
            content_store: Arc::new(RwLock::new(content_store)),
            #[cfg(feature = "advanced-security")]
            bloom_filter,
        })
    }

    pub fn save(&self) -> Result<()> {
        let state = RegistryState {
            capsules: self.capsules.read().unwrap().clone(),
            next_segment_id: *self.next_segment_id.read().unwrap(),
            content_store: self.content_store.read().unwrap().clone(),
        };

        let json = serde_json::to_string_pretty(&state)?;
        fs::write(&self.metadata_path, json)?;
        Ok(())
    }

    pub fn create_capsule_with_segments(
        &self,
        id: CapsuleId,
        size: u64,
        segments: Vec<SegmentId>,
        policy: Policy,
    ) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();

        if capsules.contains_key(&id) {
            anyhow::bail!("Capsule collision (extremely unlikely)");
        }

        let capsule = Capsule {
            id,
            size,
            segments,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            policy,
            deduped_bytes: 0, // Will be updated during write
        };

        capsules.insert(id, capsule);
        drop(capsules);
        self.save()?;
        Ok(())
    }

    pub fn lookup(&self, id: CapsuleId) -> Result<Capsule> {
        self.capsules
            .read()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))
    }

    pub fn alloc_segment(&self) -> SegmentId {
        let mut next = self.next_segment_id.write().unwrap();
        let id = *next;
        *next += 1;
        SegmentId(id)
    }

    pub fn add_segment(&self, capsule_id: CapsuleId, seg_id: SegmentId) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();
        let capsule = capsules
            .get_mut(&capsule_id)
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))?;
        capsule.segments.push(seg_id);
        drop(capsules);
        self.save()?;
        Ok(())
    }

    // NEW: Phase 2.2 - Deduplication methods

    /// Check if content hash already exists in store
    pub fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        #[cfg(feature = "advanced-security")]
        if let Some(filter) = &self.bloom_filter {
            if !filter.might_contain(hash) {
                return None;
            }
        }
        self.content_store.read().unwrap().get(hash).copied()
    }

    /// Register new content hash → segment mapping
    pub fn register_content(&self, hash: ContentHash, seg_id: SegmentId) -> Result<()> {
        self.content_store
            .write()
            .unwrap()
            .insert(hash.clone(), seg_id);
        #[cfg(feature = "advanced-security")]
        if let Some(filter) = &self.bloom_filter {
            filter.record_insertion(&hash);
        }
        self.save()?;
        Ok(())
    }

    pub fn deregister_content(&self, hash: &ContentHash, seg_id: SegmentId) -> Result<bool> {
        let mut store = self.content_store.write().unwrap();
        if let Some(current) = store.get(hash) {
            if *current == seg_id {
                store.remove(hash);
                #[cfg(feature = "advanced-security")]
                if let Some(filter) = &self.bloom_filter {
                    filter.record_removal(hash);
                }
                drop(store);
                self.save()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Increment dedup bytes counter for a capsule
    pub fn add_deduped_bytes(&self, capsule_id: CapsuleId, bytes: u64) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();
        if let Some(capsule) = capsules.get_mut(&capsule_id) {
            capsule.deduped_bytes += bytes;
        }
        Ok(())
    }

    pub fn list_capsules(&self) -> Vec<CapsuleId> {
        self.capsules.read().unwrap().keys().copied().collect()
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        let capsule = self
            .capsules
            .write()
            .unwrap()
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))?;
        self.save()?;
        Ok(capsule)
    }

    /// Get dedup statistics (for debugging/monitoring)
    pub fn get_dedup_stats(&self) -> (usize, usize) {
        let content_store = self.content_store.read().unwrap();
        let capsules = self.capsules.read().unwrap();

        let total_segments: usize = capsules.values().map(|c| c.segments.len()).sum();

        let unique_segments = content_store.len();

        (total_segments, unique_segments)
    }

    #[cfg(feature = "advanced-security")]
    fn configure_bloom(
        existing: Option<&HashMap<ContentHash, SegmentId>>,
    ) -> Option<Arc<BloomFilterWrapper>> {
        let capacity = std::env::var("SPACE_BLOOM_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10_000_000);
        let fp_rate = std::env::var("SPACE_BLOOM_FPR")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.001);

        let filter = if let Some(store) = existing {
            let hashes = store.keys().cloned().collect::<Vec<_>>();
            BloomFilterWrapper::with_existing(capacity, fp_rate, hashes)
        } else {
            BloomFilterWrapper::new(capacity, fp_rate)
        };

        Some(Arc::new(filter))
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
            capsules: Arc::clone(&self.capsules),
            next_segment_id: Arc::clone(&self.next_segment_id),
            metadata_path: self.metadata_path.clone(),
            content_store: Arc::clone(&self.content_store),
            #[cfg(feature = "advanced-security")]
            bloom_filter: self.bloom_filter.clone(),
        }
    }
}

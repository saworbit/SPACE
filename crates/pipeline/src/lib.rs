use std::borrow::Cow;

use anyhow::{anyhow, Context, Result};
use common::{
    traits::{
        CapsuleCatalog, Compressor, DedupStats, Deduper, Encryptor, EncryptionSummary, Keyring,
        PolicyEvaluator, StorageBackend, StorageTransaction,
    },
    Capsule, CapsuleId, CompressionPolicy, ContentHash, EncryptionPolicy, Policy, Segment,
    SegmentId,
};
use compression::Lz4ZstdCompressor;
use dedup::Blake3Deduper;
use storage::{InMemoryBackend, NvramBackend};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tracing::instrument;

use blake3;
use encryption::{
    compute_mac, derive_tweak_from_hash, encrypt_segment, keymanager::MASTER_KEY_SIZE, KeyManager,
};

/// Minimal encryptor that performs no-op transformations.
#[derive(Default, Clone)]
pub struct NoopEncryptor;

impl Encryptor for NoopEncryptor {
    fn encrypt(
        &self,
        data: Cow<'_, [u8]>,
        _policy: &EncryptionPolicy,
        _segment: SegmentId,
    ) -> Result<(Vec<u8>, EncryptionSummary)> {
        let summary = EncryptionSummary::new("noop");
        Ok((data.into_owned(), summary))
    }

    fn decrypt(
        &self,
        data: &[u8],
        _policy: &EncryptionPolicy,
        _segment: SegmentId,
    ) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn compute_mac(&self, _data: &[u8], _segment: SegmentId) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn verify_mac(&self, _data: &[u8], _mac: &[u8], _segment: SegmentId) -> Result<()> {
        Ok(())
    }
}

/// Real encryptor backed by the encryption crate.
#[derive(Clone)]
pub struct XtsEncryptor {
    key_manager: Arc<Mutex<KeyManager>>,
}

impl XtsEncryptor {
    pub fn new(key_manager: Arc<Mutex<KeyManager>>) -> Self {
        Self { key_manager }
    }

    fn acquire_key(&self, requested: Option<u32>) -> Result<(u32, encryption::XtsKeyPair)> {
        let mut manager = self
            .key_manager
            .lock()
            .map_err(|_| anyhow!("key manager mutex poisoned"))?;
        let key_version = requested.unwrap_or_else(|| manager.current_version());
        let key_pair = manager
            .get_key(key_version)
            .context("failed to load XTS key")?
            .clone();
        Ok((key_version, key_pair))
    }
}

impl Default for XtsEncryptor {
    fn default() -> Self {
        let master = [0u8; MASTER_KEY_SIZE];
        let manager = KeyManager::new(master);
        Self::new(Arc::new(Mutex::new(manager)))
    }
}

impl Encryptor for XtsEncryptor {
    fn encrypt(
        &self,
        data: Cow<'_, [u8]>,
        policy: &EncryptionPolicy,
        _segment: SegmentId,
    ) -> Result<(Vec<u8>, EncryptionSummary)> {
        if !policy.is_enabled() {
            let mut summary = EncryptionSummary::new("none");
            summary.encryption_version = None;
            return Ok((data.into_owned(), summary));
        }

        let (key_version, key_pair) = self.acquire_key(policy.key_version())?;
        let hash = blake3::hash(data.as_ref());
        let tweak = derive_tweak_from_hash(hash.as_bytes());

        let (ciphertext, mut metadata) =
            encrypt_segment(data.as_ref(), &key_pair, key_version, tweak)
            .context("segment encryption failed")?;

        let mac = compute_mac(&ciphertext, &metadata, key_pair.key1(), key_pair.key2())
            .context("failed to compute MAC")?;
        metadata.set_integrity_tag(mac);

        let algorithm = if policy.is_enabled() {
            "xts-aes-256"
        } else {
            "none"
        };
        let mut summary = EncryptionSummary::new(algorithm);
        summary.key_version = metadata.key_version;
        summary.encryption_version = metadata.encryption_version;
        summary.tweak_nonce = metadata.tweak_nonce;
        summary.integrity_tag = metadata.integrity_tag;
        summary.mac = metadata.integrity_tag.map(|tag| tag.to_vec());

        Ok((ciphertext, summary))
    }

    fn decrypt(
        &self,
        data: &[u8],
        _policy: &EncryptionPolicy,
        _segment: SegmentId,
    ) -> Result<Vec<u8>> {
        // Decryption requires persisted metadata. Placeholder implementation returns ciphertext.
        Ok(data.to_vec())
    }

    fn compute_mac(&self, data: &[u8], _segment: SegmentId) -> Result<Vec<u8>> {
        Ok(blake3::hash(data).as_bytes().to_vec())
    }

    fn verify_mac(&self, _data: &[u8], _mac: &[u8], _segment: SegmentId) -> Result<()> {
        Ok(())
    }
}

/// Basic policy evaluator that mirrors incoming policy decisions.
#[derive(Default, Clone)]
pub struct DefaultPolicyEvaluator;

impl PolicyEvaluator for DefaultPolicyEvaluator {
    fn evaluate_compression(
        &self,
        policy: &Policy,
        _sample: &[u8],
    ) -> Result<CompressionPolicy> {
        Ok(policy.compression.clone())
    }

    fn evaluate_dedup(&self, policy: &Policy) -> Result<bool> {
        Ok(policy.dedupe)
    }

    fn evaluate_encryption(&self, policy: &Policy) -> Result<EncryptionPolicy> {
        Ok(policy.encryption.clone())
    }

    fn evaluate_replication(
        &self,
        _policy: &Policy,
    ) -> Result<common::traits::ReplicationStrategy> {
        Ok(common::traits::ReplicationStrategy::default())
    }
}

/// In-memory keyring placeholder.
#[derive(Default, Clone)]
pub struct NullKeyring;

impl Keyring for NullKeyring {
    fn derive_key(&self, _capsule: CapsuleId, _segment: SegmentId) -> Result<[u8; 32]> {
        Ok([0u8; 32])
    }

    fn rotate_key(&mut self, _capsule: CapsuleId) -> Result<()> {
        Ok(())
    }
}

/// Keyring backed by the encryption key manager.
#[derive(Clone)]
pub struct KeyManagerKeyring {
    manager: Arc<Mutex<KeyManager>>,
}

impl KeyManagerKeyring {
    pub fn new(manager: Arc<Mutex<KeyManager>>) -> Self {
        Self { manager }
    }
}

impl Default for KeyManagerKeyring {
    fn default() -> Self {
        let master = [0u8; MASTER_KEY_SIZE];
        Self::new(Arc::new(Mutex::new(KeyManager::new(master))))
    }
}

impl Keyring for KeyManagerKeyring {
    fn derive_key(&self, _capsule: CapsuleId, _segment: SegmentId) -> Result<[u8; 32]> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| anyhow!("key manager mutex poisoned"))?;
        let version = manager.current_version();
        let key_pair = manager
            .get_key(version)
            .context("failed to load key for derivation")?;
        Ok(*key_pair.key1())
    }

    fn rotate_key(&mut self, _capsule: CapsuleId) -> Result<()> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| anyhow!("key manager mutex poisoned"))?;
        manager.rotate().context("key rotation failed")?;
        Ok(())
    }
}

/// Simple in-memory catalog for tests and defaults.
#[derive(Default, Clone)]
pub struct InMemoryCatalog {
    inner: Arc<Mutex<CatalogInner>>,
}

#[derive(Default)]
struct CatalogInner {
    next_segment: u64,
    capsules: HashMap<CapsuleId, Capsule>,
    content: HashMap<ContentHash, SegmentId>,
}

impl InMemoryCatalog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CapsuleCatalog for InMemoryCatalog {
    fn allocate_segment(&self) -> Result<SegmentId> {
        let mut inner = self.inner.lock().unwrap();
        let seg = SegmentId(inner.next_segment);
        inner.next_segment += 1;
        Ok(seg)
    }

    fn lookup_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.inner
            .lock()
            .unwrap()
            .capsules
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("capsule {:?} not found", id))
    }

    fn create_capsule(
        &self,
        id: CapsuleId,
        size: u64,
        policy: &Policy,
        segments: Vec<SegmentId>,
        stats: &DedupStats,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let capsule = Capsule {
            id,
            size,
            segments,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            policy: policy.clone(),
            deduped_bytes: stats.bytes_saved,
        };
        inner.capsules.insert(id, capsule);
        Ok(())
    }

    fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.inner
            .lock()
            .unwrap()
            .capsules
            .remove(&id)
            .ok_or_else(|| anyhow!("capsule {:?} not found", id))
    }

    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.inner.lock().unwrap().content.get(hash).copied()
    }

    fn register_content(&self, hash: ContentHash, segment: SegmentId) -> Result<()> {
        self.inner.lock().unwrap().content.insert(hash, segment);
        Ok(())
    }

    fn deregister_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.content.get(hash) {
            if *existing == segment {
                inner.content.remove(hash);
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn capsules(&self) -> Vec<Capsule> {
        self.inner
            .lock()
            .unwrap()
            .capsules
            .values()
            .cloned()
            .collect()
    }

    fn content_entries(&self) -> Vec<(ContentHash, SegmentId)> {
        self.inner
            .lock()
            .unwrap()
            .content
            .iter()
            .map(|(hash, seg)| (hash.clone(), *seg))
            .collect()
    }
}

/// Pipeline orchestrator that composes the modular traits.
pub struct Pipeline<C, D, E, S, Eval, K, R>
where
    C: Compressor,
    D: Deduper,
    E: Encryptor,
    S: StorageBackend,
    Eval: PolicyEvaluator,
    K: Keyring,
    R: CapsuleCatalog,
{
    compressor: C,
    deduper: D,
    encryptor: E,
    storage: S,
    evaluator: Eval,
    keyring: Option<K>,
    stats: DedupStats,
    catalog: R,
}

impl<C, D, E, S, Eval, K, R> Pipeline<C, D, E, S, Eval, K, R>
where
    C: Compressor,
    D: Deduper,
    E: Encryptor,
    S: StorageBackend,
    Eval: PolicyEvaluator,
    K: Keyring,
    R: CapsuleCatalog,
{
    pub fn new(
        compressor: C,
        deduper: D,
        encryptor: E,
        storage: S,
        evaluator: Eval,
        keyring: Option<K>,
        catalog: R,
    ) -> Self {
        Self {
            compressor,
            deduper,
            encryptor,
            storage,
            evaluator,
            keyring,
            stats: DedupStats::default(),
            catalog,
        }
    }

    #[instrument(skip_all)]
    pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        let capsule_id = CapsuleId::new();
        let compression_policy = self
            .evaluator
            .evaluate_compression(policy, &data[..data.len().min(1024)])?;

        let (view, summary) = self.compressor.compress(data, &compression_policy)?;
        let hash = self.deduper.hash_content(view.as_ref());

        let mut segment_ids = Vec::new();
        let mut dedup_stats = DedupStats::new();

        if let Some(existing) = self.catalog.lookup_content(&hash) {
            let mut metadata = self.storage.metadata(existing).await?;
            metadata.ref_count += 1;
            metadata.deduplicated = metadata.ref_count > 1;
            let mut txn = self.storage.begin_txn().await?;
            txn.set_segment_metadata(existing, metadata).await?;
            txn.commit().await?;
            self.deduper.update_stats(summary.output_size as u64, true);
            self.stats.record(summary.output_size as u64, true);
            dedup_stats.record(summary.output_size as u64, true);
            segment_ids.push(existing);
        } else {
            let mut txn = self.storage.begin_txn().await?;
            let seg_id = self.catalog.allocate_segment()?;

            let encryption_policy = self.evaluator.evaluate_encryption(policy)?;
            let (payload, encryption_summary) = if encryption_policy.is_enabled() {
                let _key = self
                    .keyring
                    .as_ref()
                    .map(|keyring| keyring.derive_key(capsule_id, seg_id))
                    .transpose()?;
                let (encrypted, summary) = self
                    .encryptor
                    .encrypt(Cow::Borrowed(view.as_ref()), &encryption_policy, seg_id)?;
                (encrypted, summary)
            } else {
                (view.into_owned(), EncryptionSummary::new("none"))
            };

            txn.append(seg_id, &payload).await?;
            let metadata = Segment {
                id: seg_id,
                offset: 0,
                len: payload.len() as u32,
                compressed: summary.compressed,
                compression_algo: summary.algorithm.clone(),
                content_hash: Some(hash.clone()),
                ref_count: 1,
                deduplicated: false,
                access_count: 0,
                encryption_version: encryption_summary.encryption_version,
                key_version: encryption_summary.key_version,
                tweak_nonce: encryption_summary.tweak_nonce,
                integrity_tag: encryption_summary.integrity_tag,
                encrypted: encryption_policy.is_enabled(),
            };
            txn.set_segment_metadata(seg_id, metadata).await?;
            txn.commit().await?;

            self.catalog.register_content(hash.clone(), seg_id)?;
            self.deduper.register_content(hash, seg_id)?;
            self.deduper.update_stats(summary.output_size as u64, false);
            self.stats.record(summary.output_size as u64, false);
            dedup_stats.record(summary.output_size as u64, false);
            segment_ids.push(seg_id);
        }

        self.catalog.create_capsule(
            capsule_id,
            data.len() as u64,
            policy,
            segment_ids,
            &dedup_stats,
        )?;

        Ok(capsule_id)
    }

    pub fn stats(&self) -> DedupStats {
        self.stats.clone()
    }

    pub async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.catalog.lookup_capsule(id)?;
        let mut output = Vec::with_capacity(capsule.size as usize);

        for seg_id in &capsule.segments {
            let metadata = self.storage.metadata(*seg_id).await?;
            let raw = self.storage.read(*seg_id).await?;
            let decrypted = if metadata.encrypted {
                self.encryptor
                    .decrypt(&raw, &capsule.policy.encryption, *seg_id)?
            } else {
                raw
            };
            let decompressed = if metadata.compressed {
                self.compressor
                    .decompress(&decrypted, metadata.compression_algo.as_str())?
            } else {
                decrypted
            };
            output.extend_from_slice(&decompressed);
        }

        Ok(output)
    }

    pub async fn delete_capsule(&mut self, id: CapsuleId) -> Result<()> {
        let capsule = self.catalog.lookup_capsule(id)?;

        for seg_id in &capsule.segments {
            let metadata = self.storage.metadata(*seg_id).await?;
            let mut updated = metadata.clone();

            if updated.ref_count > 1 {
                updated.ref_count -= 1;
                updated.deduplicated = updated.ref_count > 1;
                let mut txn = self.storage.begin_txn().await?;
                txn.set_segment_metadata(*seg_id, updated).await?;
                txn.commit().await?;
            } else {
                self.storage.delete(*seg_id).await?;
                if let Some(hash) = metadata.content_hash {
                    let _ = self.catalog.deregister_content(&hash, *seg_id)?;
                }
            }
        }

        self.catalog.delete_capsule(id)?;
        Ok(())
    }

    pub async fn garbage_collect(&mut self) -> Result<usize> {
        let referenced: HashSet<SegmentId> = self
            .catalog
            .capsules()
            .into_iter()
            .flat_map(|capsule| capsule.segments.into_iter())
            .collect();

        let content_map: HashMap<SegmentId, ContentHash> = self
            .catalog
            .content_entries()
            .into_iter()
            .map(|(hash, seg)| (seg, hash))
            .collect();

        let mut reclaimed = 0usize;

        let orphan_segments = self.storage.segment_ids().await?;
        let mut txn = self.storage.begin_txn().await?;

        for seg_id in orphan_segments {
            let metadata = match self.storage.metadata(seg_id).await {
                Ok(meta) => meta,
                Err(_) => continue,
            };

            if referenced.contains(&seg_id) && metadata.ref_count > 0 {
                continue;
            }

            txn.delete(seg_id).await?;
            if let Some(hash) = content_map.get(&seg_id) {
                let _ = self.catalog.deregister_content(hash, seg_id)?;
            }
            reclaimed += 1;
        }
        txn.commit().await?;

        Ok(reclaimed)
    }
}

/// Builder used to assemble pipelines with optional overrides.
pub struct PipelineBuilder<
    C = Lz4ZstdCompressor,
    D = Blake3Deduper,
    E = NoopEncryptor,
    S = InMemoryBackend,
    Eval = DefaultPolicyEvaluator,
    K = NullKeyring,
    R = InMemoryCatalog,
> where
    C: Compressor + Default,
    D: Deduper + Default,
    E: Encryptor + Default,
    S: StorageBackend + Default,
    Eval: PolicyEvaluator + Default,
    K: Keyring + Default,
    R: CapsuleCatalog + Default,
{
    compressor: Option<C>,
    deduper: Option<D>,
    encryptor: Option<E>,
    storage: Option<S>,
    evaluator: Option<Eval>,
    keyring: Option<K>,
    catalog: Option<R>,
}

impl<
        C,
        D,
        E,
        S,
        Eval,
        K,
        R,
    > Default
    for PipelineBuilder<C, D, E, S, Eval, K, R>
where
    C: Compressor + Default,
    D: Deduper + Default,
    E: Encryptor + Default,
    S: StorageBackend + Default,
    Eval: PolicyEvaluator + Default,
    K: Keyring + Default,
    R: CapsuleCatalog + Default,
{
    fn default() -> Self {
        Self {
            compressor: None,
            deduper: None,
            encryptor: None,
            storage: None,
            evaluator: None,
            keyring: None,
            catalog: None,
        }
    }
}

impl<
        C,
        D,
        E,
        S,
        Eval,
        K,
        R,
    > PipelineBuilder<C, D, E, S, Eval, K, R>
where
    C: Compressor + Default,
    D: Deduper + Default,
    E: Encryptor + Default,
    S: StorageBackend + Default,
    Eval: PolicyEvaluator + Default,
    K: Keyring + Default,
    R: CapsuleCatalog + Default,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_compressor(mut self, compressor: C) -> Self {
        self.compressor = Some(compressor);
        self
    }

    pub fn with_deduper(mut self, deduper: D) -> Self {
        self.deduper = Some(deduper);
        self
    }

    pub fn with_encryptor(mut self, encryptor: E) -> Self {
        self.encryptor = Some(encryptor);
        self
    }

    pub fn with_storage(mut self, storage: S) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn with_evaluator(mut self, evaluator: Eval) -> Self {
        self.evaluator = Some(evaluator);
        self
    }

    pub fn with_keyring(mut self, keyring: K) -> Self {
        self.keyring = Some(keyring);
        self
    }

    pub fn with_catalog(mut self, catalog: R) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn build(self) -> Pipeline<C, D, E, S, Eval, K, R> {
        Pipeline::new(
            self.compressor.unwrap_or_default(),
            self.deduper.unwrap_or_default(),
            self.encryptor.unwrap_or_default(),
            self.storage.unwrap_or_default(),
            self.evaluator.unwrap_or_default(),
            self.keyring,
            self.catalog.unwrap_or_default(),
        )
    }
}

pub type InMemoryPipeline = Pipeline<
    Lz4ZstdCompressor,
    Blake3Deduper,
    NoopEncryptor,
    InMemoryBackend,
    DefaultPolicyEvaluator,
    NullKeyring,
    InMemoryCatalog,
>;

pub type DefaultPipeline = InMemoryPipeline;

pub type NvramPipeline = Pipeline<
    Lz4ZstdCompressor,
    Blake3Deduper,
    NoopEncryptor,
    NvramBackend,
    DefaultPolicyEvaluator,
    NullKeyring,
    InMemoryCatalog,
>;

pub type NvramPipelineWithEncryption = Pipeline<
    Lz4ZstdCompressor,
    Blake3Deduper,
    XtsEncryptor,
    NvramBackend,
    DefaultPolicyEvaluator,
    KeyManagerKeyring,
    InMemoryCatalog,
>;

pub fn pipeline_with_nvram<P: AsRef<std::path::Path>>(path: P) -> Result<NvramPipeline> {
    let storage = NvramBackend::open(path)?;
    Ok(Pipeline::new(
        Lz4ZstdCompressor::default(),
        Blake3Deduper::default(),
        NoopEncryptor::default(),
        storage,
        DefaultPolicyEvaluator::default(),
        None,
        InMemoryCatalog::default(),
    ))
}

pub fn pipeline_with_nvram_xts<P: AsRef<std::path::Path>>(
    path: P,
    key_manager: Arc<Mutex<KeyManager>>,
) -> Result<NvramPipelineWithEncryption> {
    let storage = NvramBackend::open(path)?;
    Ok(Pipeline::new(
        Lz4ZstdCompressor::default(),
        Blake3Deduper::default(),
        XtsEncryptor::new(Arc::clone(&key_manager)),
        storage,
        DefaultPolicyEvaluator::default(),
        Some(KeyManagerKeyring::new(key_manager)),
        InMemoryCatalog::default(),
    ))
}

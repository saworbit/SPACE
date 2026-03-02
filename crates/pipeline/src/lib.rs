use std::borrow::Cow;

use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use bytes::Bytes;
use common::{
    traits::{
        CapsuleCatalog, Compressor, DataStream, DecryptContext, DedupStats, Deduper,
        EncryptionSummary, Encryptor, Keyring, PolicyEvaluator, StorageBackend, StorageTransaction,
    },
    Capsule, CapsuleId, CompressionPolicy, ContentHash, EncryptionPolicy, Policy, Segment,
    SegmentId,
};
use compression::Lz4ZstdCompressor;
use dedup::Blake3Deduper;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use storage::{InMemoryBackend, NvramBackend};
use tracing::instrument;

use encryption::{
    compute_mac, derive_tweak_from_hash, encrypt_segment, keymanager::MASTER_KEY_SIZE,
    EncryptionMetadata, KeyManager,
};
use layout_engine::LayoutEngine;
#[cfg(feature = "phase5")]
use std::sync::OnceLock;

#[cfg(feature = "phase5")]
use common::{TransformDef, TransformTrigger};
#[cfg(feature = "phase5")]
use futures::future::BoxFuture;
#[cfg(feature = "phase5")]
use transform_engine::{ModuleResolver, TransformEngine};
#[cfg(feature = "phase5")]
use uuid::Uuid;

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
        _ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn compute_mac(
        &self,
        _data: &[u8],
        _segment: SegmentId,
        _ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn verify_mac(&self, _data: &[u8], _segment: SegmentId, _ctx: &DecryptContext) -> Result<()> {
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

    fn resolve_tweak(&self, ctx: &DecryptContext) -> Result<[u8; 16]> {
        ctx.tweak_nonce
            .or_else(|| ctx.content_hash.map(|hash| derive_tweak_from_hash(&hash)))
            .ok_or_else(|| anyhow!("missing tweak nonce or content hash for decryption"))
    }

    fn build_mac_metadata(
        &self,
        ctx: &DecryptContext,
        ciphertext_len: u32,
        integrity_tag: Option<[u8; 16]>,
        policy: Option<&EncryptionPolicy>,
    ) -> Result<(EncryptionMetadata, encryption::XtsKeyPair)> {
        let tweak = self.resolve_tweak(ctx)?;
        let (key_version, key_pair) = self.acquire_key(ctx.key_version)?;
        let encryption_version = ctx
            .encryption_version
            .or_else(|| policy.and_then(|p| p.is_enabled().then_some(1)));
        let metadata = EncryptionMetadata {
            encryption_version,
            key_version: Some(key_version),
            wrapped_segment_key: None,
            tweak_nonce: Some(tweak),
            integrity_tag,
            ciphertext_len: Some(ciphertext_len),
        };
        Ok((metadata, key_pair))
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
        policy: &EncryptionPolicy,
        _segment: SegmentId,
        ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        let ciphertext_len = ctx.ciphertext_len.unwrap_or(data.len() as u32);
        let (metadata, key_pair) =
            self.build_mac_metadata(ctx, ciphertext_len, ctx.integrity_tag, Some(policy))?;
        encryption::verify_mac(data, &metadata, key_pair.key1(), key_pair.key2())
            .context("MAC verification failed")?;

        let tweak = metadata
            .tweak_nonce
            .ok_or_else(|| anyhow!("missing tweak nonce for decryption"))?;
        encryption::decrypt(data, &key_pair, &tweak).context("XTS-AES-256 decryption failed")
    }

    fn compute_mac(
        &self,
        data: &[u8],
        _segment: SegmentId,
        ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        let (metadata, key_pair) = self.build_mac_metadata(ctx, data.len() as u32, None, None)?;
        let tag = encryption::compute_mac(data, &metadata, key_pair.key1(), key_pair.key2())
            .context("failed to compute MAC")?;
        Ok(tag.to_vec())
    }

    fn verify_mac(&self, data: &[u8], _segment: SegmentId, ctx: &DecryptContext) -> Result<()> {
        let (metadata, key_pair) =
            self.build_mac_metadata(ctx, data.len() as u32, ctx.integrity_tag, None)?;
        encryption::verify_mac(data, &metadata, key_pair.key1(), key_pair.key2())
            .context("MAC verification failed")?;
        Ok(())
    }
}

/// Basic policy evaluator that mirrors incoming policy decisions.
#[derive(Default, Clone)]
pub struct DefaultPolicyEvaluator;

impl PolicyEvaluator for DefaultPolicyEvaluator {
    fn evaluate_compression(&self, policy: &Policy, _sample: &[u8]) -> Result<CompressionPolicy> {
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .content
            .get(hash)
            .copied()
    }

    fn register_content(&self, hash: ContentHash, segment: SegmentId) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .content
            .insert(hash, segment);
        Ok(())
    }

    fn deregister_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(feature = "phase5")]
fn wasm_engine() -> Result<&'static TransformEngine> {
    static ENGINE: OnceLock<Result<TransformEngine>> = OnceLock::new();
    match ENGINE.get_or_init(TransformEngine::new) {
        Ok(engine) => Ok(engine),
        Err(err) => Err(anyhow!(err.to_string())).context("init wasm transform engine"),
    }
}

#[cfg(feature = "phase5")]
struct PipelineModuleResolver<'a, C, E, S, R> {
    compressor: &'a C,
    encryptor: &'a E,
    storage: &'a S,
    catalog: &'a R,
}

#[cfg(feature = "phase5")]
impl<'a, C, E, S, R> PipelineModuleResolver<'a, C, E, S, R>
where
    C: Compressor + Send + Sync,
    E: Encryptor + Send + Sync,
    S: StorageBackend + Send + Sync,
    R: CapsuleCatalog + Send + Sync,
{
    async fn read_capsule_bytes(&self, capsule_id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.catalog.lookup_capsule(capsule_id)?;
        let mut out = Vec::with_capacity(capsule.size as usize);

        for seg_id in &capsule.segments {
            let metadata = self.storage.metadata(*seg_id).await?;
            let raw = self.storage.read(*seg_id).await?;

            let decrypted = if metadata.encrypted {
                let ctx = DecryptContext::from_segment(&metadata);
                self.encryptor
                    .decrypt(&raw, &capsule.policy.encryption, *seg_id, &ctx)?
            } else {
                raw
            };

            let decompressed = if metadata.compressed {
                self.compressor
                    .decompress(&decrypted, metadata.compression_algo.as_str())?
            } else {
                decrypted
            };

            out.extend_from_slice(&decompressed);
        }

        Ok(out)
    }

    async fn read_file(image: &str) -> Result<Vec<u8>> {
        let path = if let Some(rest) = image.strip_prefix("file://") {
            rest.trim_start_matches('/').to_string()
        } else {
            image.to_string()
        };
        std::fs::read(&path).with_context(|| format!("read wasm module at {path}"))
    }
}

#[cfg(feature = "phase5")]
impl<'a, C, E, S, R> ModuleResolver for PipelineModuleResolver<'a, C, E, S, R>
where
    C: Compressor + Send + Sync,
    E: Encryptor + Send + Sync,
    S: StorageBackend + Send + Sync,
    R: CapsuleCatalog + Send + Sync,
{
    fn load<'b>(&'b self, image: &'b str) -> BoxFuture<'b, Result<Vec<u8>>> {
        Box::pin(async move {
            if let Some(rest) = image.strip_prefix("capsule://") {
                let id = rest.trim_matches('/');
                let uuid = Uuid::parse_str(id).with_context(|| {
                    format!("invalid capsule URI (expected capsule://<UUID>): {image}")
                })?;
                return self.read_capsule_bytes(CapsuleId::from_uuid(uuid)).await;
            }

            Self::read_file(image).await
        })
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

    #[cfg(feature = "phase5")]
    async fn apply_on_write_transforms(&self, data: &[u8], policy: &Policy) -> Result<Vec<u8>>
    where
        C: Compressor + Clone + Send + Sync + 'static,
        D: Deduper + Send + Sync + 'static,
        E: Encryptor + Clone + Send + Sync + 'static,
        S: StorageBackend + Clone + Send + Sync + 'static,
        Eval: PolicyEvaluator + Send + Sync + 'static,
        K: Keyring + Send + Sync + 'static,
        R: CapsuleCatalog + Send + Sync + 'static,
    {
        let transforms: Vec<TransformDef> = policy
            .transform
            .iter()
            .filter(|t| t.trigger == TransformTrigger::OnWrite)
            .cloned()
            .collect();

        if transforms.is_empty() {
            return Ok(data.to_vec());
        }

        let engine = wasm_engine()?;
        let resolver = PipelineModuleResolver {
            compressor: &self.compressor,
            encryptor: &self.encryptor,
            storage: &self.storage,
            catalog: &self.catalog,
        };

        let chunk_bytes = 4 * 1024 * 1024;
        let base = futures::stream::iter(
            data.chunks(chunk_bytes)
                .map(|chunk| Ok::<Bytes, anyhow::Error>(Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>(),
        );

        let mut stream: DataStream = Box::pin(base);
        for def in transforms {
            stream = engine.execute_stream(stream, def, &resolver).await?;
        }

        let mut out = Vec::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    #[cfg(feature = "phase5")]
    #[instrument(skip_all)]
    pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId>
    where
        C: Compressor + Clone + Send + Sync + 'static,
        D: Deduper + Send + Sync + 'static,
        E: Encryptor + Clone + Send + Sync + 'static,
        S: StorageBackend + Clone + Send + Sync + 'static,
        Eval: PolicyEvaluator + Send + Sync + 'static,
        K: Keyring + Send + Sync + 'static,
        R: CapsuleCatalog + Send + Sync + 'static,
    {
        let transformed_data;
        let data = if policy
            .transform
            .iter()
            .any(|t| t.trigger == TransformTrigger::OnWrite)
        {
            transformed_data = self.apply_on_write_transforms(data, policy).await?;
            transformed_data.as_slice()
        } else {
            data
        };

        let capsule_id = CapsuleId::new();
        let compression_policy = self
            .evaluator
            .evaluate_compression(policy, &data[..data.len().min(1024)])?;
        let chunk_size = policy.layout.strategy.default_segment_size();
        let data_slices: Vec<&[u8]> = data.chunks(chunk_size).collect();
        let layout_engine = LayoutEngine::new(policy);
        let zone_plan = layout_engine.synthesize(&[capsule_id], &data_slices, policy)?;
        let encryption_policy = self.evaluator.evaluate_encryption(policy)?;

        let mut segment_ids = Vec::new();
        let mut dedup_stats = DedupStats::new();

        let mut planned_segments: Vec<(u64, u64)> = zone_plan
            .zones
            .iter()
            .flat_map(|zone| zone.segments.iter().map(|seg| (seg.offset, seg.length)))
            .collect();
        planned_segments.sort_by_key(|(offset, _)| *offset);

        for (offset, length) in planned_segments {
            let start = offset as usize;
            let end = start.saturating_add(length as usize);
            if end > data.len() {
                return Err(anyhow!(
                    "zone plan refers beyond input data: {} > {}",
                    end,
                    data.len()
                ));
            }
            let chunk = &data[start..end];
            let (view, summary) = self.compressor.compress(chunk, &compression_policy)?;
            let hash = self.deduper.hash_content(view.as_ref());

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

                let (payload, encryption_summary) = if encryption_policy.is_enabled() {
                    let _key = self
                        .keyring
                        .as_ref()
                        .map(|keyring| keyring.derive_key(capsule_id, seg_id))
                        .transpose()?;
                    let (encrypted, summary) = self.encryptor.encrypt(
                        Cow::Borrowed(view.as_ref()),
                        &encryption_policy,
                        seg_id,
                    )?;
                    (encrypted, summary)
                } else {
                    (view.into_owned(), EncryptionSummary::new("none"))
                };

                txn.append(seg_id, &payload).await?;
                let metadata = Segment {
                    id: seg_id,
                    offset: 0,
                    len: payload.len() as u32,
                    plain_len: Some(summary.original_size as u32),
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
                    pq_ciphertext: None,
                    pq_nonce: None,
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

    #[cfg(not(feature = "phase5"))]
    #[instrument(skip_all)]
    pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        let capsule_id = CapsuleId::new();
        let compression_policy = self
            .evaluator
            .evaluate_compression(policy, &data[..data.len().min(1024)])?;
        let chunk_size = policy.layout.strategy.default_segment_size();
        let data_slices: Vec<&[u8]> = data.chunks(chunk_size).collect();
        let layout_engine = LayoutEngine::new(policy);
        let zone_plan = layout_engine.synthesize(&[capsule_id], &data_slices, policy)?;
        let encryption_policy = self.evaluator.evaluate_encryption(policy)?;

        let mut segment_ids = Vec::new();
        let mut dedup_stats = DedupStats::new();

        let mut planned_segments: Vec<(u64, u64)> = zone_plan
            .zones
            .iter()
            .flat_map(|zone| zone.segments.iter().map(|seg| (seg.offset, seg.length)))
            .collect();
        planned_segments.sort_by_key(|(offset, _)| *offset);

        for (offset, length) in planned_segments {
            let start = offset as usize;
            let end = start.saturating_add(length as usize);
            if end > data.len() {
                return Err(anyhow!(
                    "zone plan refers beyond input data: {} > {}",
                    end,
                    data.len()
                ));
            }
            let chunk = &data[start..end];
            let (view, summary) = self.compressor.compress(chunk, &compression_policy)?;
            let hash = self.deduper.hash_content(view.as_ref());

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

                let (payload, encryption_summary) = if encryption_policy.is_enabled() {
                    let _key = self
                        .keyring
                        .as_ref()
                        .map(|keyring| keyring.derive_key(capsule_id, seg_id))
                        .transpose()?;
                    let (encrypted, summary) = self.encryptor.encrypt(
                        Cow::Borrowed(view.as_ref()),
                        &encryption_policy,
                        seg_id,
                    )?;
                    (encrypted, summary)
                } else {
                    (view.into_owned(), EncryptionSummary::new("none"))
                };

                txn.append(seg_id, &payload).await?;
                let metadata = Segment {
                    id: seg_id,
                    offset: 0,
                    len: payload.len() as u32,
                    plain_len: Some(summary.original_size as u32),
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
                    pq_ciphertext: None,
                    pq_nonce: None,
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

    pub async fn read_capsule_stream(&self, id: CapsuleId) -> Result<DataStream>
    where
        C: Compressor + Clone + Send + Sync + 'static,
        D: Deduper + Send + Sync + 'static,
        E: Encryptor + Clone + Send + Sync + 'static,
        S: StorageBackend + Clone + Send + Sync + 'static,
        Eval: PolicyEvaluator + Send + Sync + 'static,
        K: Keyring + Send + Sync + 'static,
        R: CapsuleCatalog + Send + Sync + 'static,
    {
        let capsule = self.catalog.lookup_capsule(id)?;
        let storage_stream = self.storage.clone();
        let encryptor_stream = self.encryptor.clone();
        let compressor_stream = self.compressor.clone();
        let encryption_policy = capsule.policy.encryption.clone();

        let stream = try_stream! {
            for seg_id in capsule.segments {
                let metadata = storage_stream.metadata(seg_id).await
                    .map_err(|e| anyhow!("Failed to fetch metadata for segment {:?}: {}", seg_id, e))?;

                let raw = storage_stream.read(seg_id).await
                    .map_err(|e| anyhow!("Failed to read segment {:?}: {}", seg_id, e))?;

                let decrypted = if metadata.encrypted {
                    let ctx = DecryptContext::from_segment(&metadata);
                    encryptor_stream
                        .decrypt(&raw, &encryption_policy, seg_id, &ctx)
                        .map_err(|e| anyhow!("Decryption failed for segment {:?}: {}", seg_id, e))?
                } else {
                    raw
                };

                let decompressed = if metadata.compressed {
                    compressor_stream
                        .decompress(&decrypted, metadata.compression_algo.as_str())
                        .map_err(|e| anyhow!("Decompression failed for segment {:?}: {}", seg_id, e))?
                } else {
                    decrypted
                };

                yield Bytes::from(decompressed);
            }
        };

        let out: DataStream = Box::pin(stream);

        #[cfg(feature = "phase5")]
        let mut out = out;

        #[cfg(feature = "phase5")]
        {
            let transforms: Vec<TransformDef> = capsule
                .policy
                .transform
                .iter()
                .filter(|t| t.trigger == TransformTrigger::OnRead)
                .cloned()
                .collect();

            if !transforms.is_empty() {
                let engine = wasm_engine()?;
                let storage_resolver = self.storage.clone();
                let encryptor_resolver = self.encryptor.clone();
                let compressor_resolver = self.compressor.clone();

                let resolver = PipelineModuleResolver {
                    compressor: &compressor_resolver,
                    encryptor: &encryptor_resolver,
                    storage: &storage_resolver,
                    catalog: &self.catalog,
                };

                for def in transforms {
                    out = engine.execute_stream(out, def, &resolver).await?;
                }
            }
        }

        Ok(out)
    }

    /// Deprecated: use `read_capsule_stream` instead.
    pub async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>>
    where
        C: Compressor + Clone + Send + Sync + 'static,
        D: Deduper + Send + Sync + 'static,
        E: Encryptor + Clone + Send + Sync + 'static,
        S: StorageBackend + Clone + Send + Sync + 'static,
        Eval: PolicyEvaluator + Send + Sync + 'static,
        K: Keyring + Send + Sync + 'static,
        R: CapsuleCatalog + Send + Sync + 'static,
    {
        let mut stream = self.read_capsule_stream(id).await?;
        let mut output = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            output.extend_from_slice(&chunk);
        }
        Ok(output)
    }

    pub async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let capsule = self.catalog.lookup_capsule(id)?;
        if offset >= capsule.size {
            return Ok(Vec::new());
        }

        let range_end = std::cmp::min(offset.saturating_add(len as u64), capsule.size);
        let mut remaining = (range_end - offset) as usize;
        let mut cursor = 0u64;
        let mut output = Vec::with_capacity(remaining);

        for seg_id in &capsule.segments {
            if remaining == 0 {
                break;
            }

            let metadata = self.storage.metadata(*seg_id).await?;
            let seg_len_hint = metadata
                .plain_len
                .map(u64::from)
                .or_else(|| (!metadata.compressed).then_some(metadata.len as u64));

            if let Some(seg_len) = seg_len_hint {
                let seg_end = cursor + seg_len;
                if seg_end <= offset {
                    cursor = seg_end;
                    continue;
                }
            }

            let raw = self.storage.read(*seg_id).await?;
            let decrypted = if metadata.encrypted {
                let ctx = DecryptContext::from_segment(&metadata);
                self.encryptor
                    .decrypt(&raw, &capsule.policy.encryption, *seg_id, &ctx)?
            } else {
                raw
            };
            let decompressed = if metadata.compressed {
                self.compressor
                    .decompress(&decrypted, metadata.compression_algo.as_str())?
            } else {
                decrypted
            };

            let seg_len = decompressed.len() as u64;
            let seg_end = cursor + seg_len;

            if seg_end <= offset {
                cursor = seg_end;
                continue;
            }

            let start = if offset > cursor {
                (offset - cursor) as usize
            } else {
                0
            };

            let take = std::cmp::min(remaining, decompressed.len().saturating_sub(start));
            output.extend_from_slice(&decompressed[start..start + take]);
            remaining -= take;
            cursor = seg_end;
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

impl<C, D, E, S, Eval, K, R> Default for PipelineBuilder<C, D, E, S, Eval, K, R>
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

impl<C, D, E, S, Eval, K, R> PipelineBuilder<C, D, E, S, Eval, K, R>
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
        Lz4ZstdCompressor,
        Blake3Deduper::default(),
        NoopEncryptor,
        storage,
        DefaultPolicyEvaluator,
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
        Lz4ZstdCompressor,
        Blake3Deduper::default(),
        XtsEncryptor::new(Arc::clone(&key_manager)),
        storage,
        DefaultPolicyEvaluator,
        Some(KeyManagerKeyring::new(key_manager)),
        InMemoryCatalog::default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::traits::DecryptContext;
    use encryption::keymanager::MASTER_KEY_SIZE;
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};

    fn make_xts_encryptor() -> XtsEncryptor {
        let master = [42u8; MASTER_KEY_SIZE];
        let manager = KeyManager::new(master);
        XtsEncryptor::new(Arc::new(Mutex::new(manager)))
    }

    fn make_xts_encryptor_with_key(key_byte: u8) -> XtsEncryptor {
        let master = [key_byte; MASTER_KEY_SIZE];
        let manager = KeyManager::new(master);
        XtsEncryptor::new(Arc::new(Mutex::new(manager)))
    }

    // ── XtsEncryptor encrypt / decrypt round-trip ────────────────────

    #[test]
    fn test_xts_encrypt_decrypt_roundtrip() {
        let enc = make_xts_encryptor();
        let plaintext = b"Hello, SPACE encryption roundtrip!".repeat(5);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);

        let (ciphertext, summary) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .expect("encrypt should succeed");

        // Ciphertext should differ from plaintext
        assert_ne!(ciphertext.as_slice(), plaintext.as_slice());
        assert_eq!(ciphertext.len(), plaintext.len());

        // Build a DecryptContext from the encryption summary
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        let decrypted = enc
            .decrypt(&ciphertext, &policy, seg, &ctx)
            .expect("decrypt should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_xts_encrypt_noop_when_policy_none() {
        let enc = make_xts_encryptor();
        let plaintext = b"No encryption please".to_vec();
        let policy = EncryptionPolicy::Disabled;
        let seg = SegmentId(1);

        let (output, summary) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .expect("encrypt should succeed");

        assert_eq!(
            output, plaintext,
            "policy None should return data unchanged"
        );
        assert_eq!(summary.algorithm, "none");
    }

    #[test]
    fn test_xts_decrypt_missing_tweak_returns_error() {
        let enc = make_xts_encryptor();
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let ctx = DecryptContext {
            encryption_version: Some(1),
            key_version: Some(1),
            tweak_nonce: None, // Missing!
            integrity_tag: None,
            ciphertext_len: Some(0),
            ..Default::default()
        };

        let result = enc.decrypt(b"some ciphertext data", &policy, seg, &ctx);
        assert!(result.is_err(), "decrypt with missing tweak should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("tweak"),
            "error should mention tweak, got: {err_msg}"
        );
    }

    #[test]
    fn test_xts_encrypt_deterministic() {
        let enc = make_xts_encryptor();
        let plaintext = b"Deterministic encryption test!".repeat(3);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(10);

        let (ct1, _) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();
        let (ct2, _) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();

        assert_eq!(
            ct1, ct2,
            "same plaintext + same key → same ciphertext (dedup-preserving)"
        );
    }

    #[test]
    fn test_xts_different_keys_produce_different_ciphertext() {
        let enc1 = make_xts_encryptor_with_key(1);
        let enc2 = make_xts_encryptor_with_key(2);
        let plaintext = b"Different key test data!".repeat(3);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);

        let (ct1, _) = enc1
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();
        let (ct2, _) = enc2
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "different keys should produce different ciphertext"
        );
    }

    #[test]
    fn test_xts_wrong_key_decrypt_fails() {
        let enc1 = make_xts_encryptor_with_key(10);
        let enc2 = make_xts_encryptor_with_key(20);
        let plaintext = b"Wrong key decryption test!".repeat(3);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);

        let (ciphertext, summary) = enc1
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();

        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        // Decrypt with a different key should fail MAC verification
        let result = enc2.decrypt(&ciphertext, &policy, seg, &ctx);
        assert!(result.is_err(), "wrong key should fail decryption");
    }

    // ── DecryptContext construction ──────────────────────────────────

    #[test]
    fn test_decrypt_context_default() {
        let ctx = DecryptContext::default();
        assert!(ctx.encryption_version.is_none());
        assert!(ctx.key_version.is_none());
        assert!(ctx.tweak_nonce.is_none());
        assert!(ctx.integrity_tag.is_none());
        assert!(ctx.ciphertext_len.is_none());
        assert!(ctx.content_hash.is_none());
    }

    #[test]
    fn test_decrypt_context_from_segment() {
        let tweak = [0xABu8; 16];
        let tag = [0xCDu8; 16];
        let content_hash = ContentHash::from_bytes(&[0x11u8; 32]);
        let seg = Segment {
            id: SegmentId(5),
            offset: 0,
            len: 1024,
            plain_len: Some(1024),
            compressed: false,
            compression_algo: String::new(),
            content_hash: Some(content_hash),
            ref_count: 1,
            deduplicated: false,
            access_count: 0,
            encryption_version: Some(1),
            key_version: Some(3),
            tweak_nonce: Some(tweak),
            integrity_tag: Some(tag),
            encrypted: true,
            pq_ciphertext: None,
            pq_nonce: None,
        };

        let ctx = DecryptContext::from_segment(&seg);
        assert_eq!(ctx.encryption_version, Some(1));
        assert_eq!(ctx.key_version, Some(3));
        assert_eq!(ctx.tweak_nonce, Some(tweak));
        assert_eq!(ctx.integrity_tag, Some(tag));
        assert_eq!(ctx.ciphertext_len, Some(1024));
        assert!(ctx.content_hash.is_some());
    }

    // ── MAC compute + verify ────────────────────────────────────────

    #[test]
    fn test_xts_compute_mac_produces_bytes() {
        let enc = make_xts_encryptor();
        let data = b"MAC test data 1234";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let (ciphertext, summary) = enc.encrypt(Cow::Borrowed(data), &policy, seg).unwrap();
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: None,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };
        let mac = enc.compute_mac(&ciphertext, seg, &ctx).unwrap();
        assert_eq!(mac.len(), 16, "MAC tag should be 16 bytes");
    }

    #[test]
    fn test_xts_verify_mac_valid() {
        let enc = make_xts_encryptor();
        let data = b"MAC verify test data";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let (ciphertext, summary) = enc.encrypt(Cow::Borrowed(data), &policy, seg).unwrap();
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        enc.verify_mac(&ciphertext, seg, &ctx)
            .expect("valid MAC should pass verification");
    }

    #[test]
    fn test_xts_verify_mac_tampered_data() {
        let enc = make_xts_encryptor();
        let data = b"Original MAC data";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let (mut ciphertext, summary) = enc.encrypt(Cow::Borrowed(data), &policy, seg).unwrap();
        ciphertext[0] ^= 0xFF;
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        let result = enc.verify_mac(&ciphertext, seg, &ctx);
        assert!(
            result.is_err(),
            "tampered data should fail MAC verification"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("MAC verification failed"),
            "error should mention MAC failure, got: {err_msg}"
        );
    }

    #[test]
    fn test_xts_verify_mac_tampered_tag() {
        let enc = make_xts_encryptor();
        let data = b"MAC tag tamper test";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let (ciphertext, summary) = enc.encrypt(Cow::Borrowed(data), &policy, seg).unwrap();
        let mut tag = summary.integrity_tag.unwrap();
        tag[0] ^= 0xFF;
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: Some(tag),
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        let result = enc.verify_mac(&ciphertext, seg, &ctx);
        assert!(result.is_err(), "tampered MAC tag should fail verification");
    }

    #[test]
    fn test_xts_verify_mac_missing_tag() {
        let enc = make_xts_encryptor();
        let data = b"Missing MAC tag!";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(1);
        let (ciphertext, summary) = enc.encrypt(Cow::Borrowed(data), &policy, seg).unwrap();
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: None,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        let result = enc.verify_mac(&ciphertext, seg, &ctx);
        assert!(result.is_err(), "missing MAC tag should fail verification");
    }

    #[test]
    fn test_xts_mac_deterministic() {
        let enc = make_xts_encryptor();
        let data = b"Deterministic MAC test";
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let (ciphertext, summary) = enc
            .encrypt(Cow::Borrowed(data), &policy, SegmentId(1))
            .unwrap();
        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: None,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };
        let mac1 = enc.compute_mac(&ciphertext, SegmentId(1), &ctx).unwrap();
        let mac2 = enc.compute_mac(&ciphertext, SegmentId(2), &ctx).unwrap();

        // MAC does not include segment ID, so tags should match for identical inputs
        assert_eq!(mac1, mac2, "MAC should be deterministic for the same data");
    }

    // ── Noop encryptor ──────────────────────────────────────────────

    #[test]
    fn test_noop_encrypt_returns_data_unchanged() {
        let enc = NoopEncryptor;
        let data = b"NoopEncryptor test";
        let policy = EncryptionPolicy::Disabled;

        let (output, summary) = enc
            .encrypt(Cow::Borrowed(data), &policy, SegmentId(1))
            .unwrap();
        assert_eq!(output, data);
        assert_eq!(summary.algorithm, "noop");
    }

    #[test]
    fn test_noop_decrypt_returns_data_unchanged() {
        let enc = NoopEncryptor;
        let data = b"NoopEncryptor decrypt";
        let ctx = DecryptContext::default();

        let result = enc
            .decrypt(data, &EncryptionPolicy::Disabled, SegmentId(1), &ctx)
            .unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn test_noop_verify_mac_always_ok() {
        let enc = NoopEncryptor;
        let ctx = DecryptContext::default();
        enc.verify_mac(b"any data", SegmentId(1), &ctx)
            .expect("noop verify_mac should always succeed");
    }

    // ── Encrypt+MAC end-to-end ──────────────────────────────────────

    #[test]
    fn test_xts_full_encrypt_mac_verify_decrypt_roundtrip() {
        let enc = make_xts_encryptor();
        let plaintext = b"Full end-to-end roundtrip test!".repeat(4);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let seg = SegmentId(99);

        // 1) Encrypt
        let (ciphertext, summary) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, seg)
            .unwrap();

        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ciphertext.len() as u32),
            ..Default::default()
        };

        // 2) Compute MAC on ciphertext
        let mac = enc.compute_mac(&ciphertext, seg, &ctx).unwrap();
        assert_eq!(mac.len(), 16);

        // 3) Verify MAC
        enc.verify_mac(&ciphertext, seg, &ctx)
            .expect("MAC on freshly-encrypted data should verify");

        // 4) Decrypt
        let decrypted = enc.decrypt(&ciphertext, &policy, seg, &ctx).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // ── XtsEncryptor::default ───────────────────────────────────────

    #[test]
    fn test_xts_encryptor_default_works() {
        let enc = XtsEncryptor::default();
        let plaintext = b"Default encryptor test".repeat(3);
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };

        let (ct, summary) = enc
            .encrypt(Cow::Borrowed(&plaintext), &policy, SegmentId(1))
            .unwrap();

        let ctx = DecryptContext {
            encryption_version: summary.encryption_version,
            key_version: summary.key_version,
            tweak_nonce: summary.tweak_nonce,
            integrity_tag: summary.integrity_tag,
            ciphertext_len: Some(ct.len() as u32),
            ..Default::default()
        };

        let decrypted = enc.decrypt(&ct, &policy, SegmentId(1), &ctx).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // ── InMemoryCatalog ───────────────────────────────────────────────

    #[test]
    fn catalog_allocate_segment_increments() {
        let catalog = InMemoryCatalog::new();
        let s1 = catalog.allocate_segment().unwrap();
        let s2 = catalog.allocate_segment().unwrap();
        let s3 = catalog.allocate_segment().unwrap();
        assert_eq!(s1.0, 0);
        assert_eq!(s2.0, 1);
        assert_eq!(s3.0, 2);
    }

    #[test]
    fn catalog_create_and_lookup_capsule() {
        use common::traits::DedupStats;
        let catalog = InMemoryCatalog::new();
        let id = CapsuleId(uuid::Uuid::new_v4());
        let policy = Policy::default();
        let stats = DedupStats::new();
        catalog
            .create_capsule(id, 100, &policy, vec![], &stats)
            .unwrap();

        let capsule = catalog.lookup_capsule(id).unwrap();
        assert_eq!(capsule.id, id);
        assert_eq!(capsule.size, 100);
    }

    #[test]
    fn catalog_lookup_missing_capsule() {
        let catalog = InMemoryCatalog::new();
        let id = CapsuleId(uuid::Uuid::new_v4());
        let result = catalog.lookup_capsule(id);
        assert!(result.is_err());
    }

    #[test]
    fn catalog_create_with_segments() {
        use common::traits::DedupStats;
        let catalog = InMemoryCatalog::new();
        let id = CapsuleId(uuid::Uuid::new_v4());
        let seg = catalog.allocate_segment().unwrap();
        let policy = Policy::default();
        let stats = DedupStats::new();
        catalog
            .create_capsule(id, 200, &policy, vec![seg], &stats)
            .unwrap();

        let capsule = catalog.lookup_capsule(id).unwrap();
        assert!(capsule.segments.contains(&seg));
    }

    #[test]
    fn catalog_delete_capsule() {
        use common::traits::DedupStats;
        let catalog = InMemoryCatalog::new();
        let id = CapsuleId(uuid::Uuid::new_v4());
        let policy = Policy::default();
        let stats = DedupStats::new();
        catalog
            .create_capsule(id, 50, &policy, vec![], &stats)
            .unwrap();
        assert!(catalog.lookup_capsule(id).is_ok());

        catalog.delete_capsule(id).unwrap();
        assert!(catalog.lookup_capsule(id).is_err());
    }

    #[test]
    fn catalog_content_dedup_mapping() {
        let catalog = InMemoryCatalog::new();
        let hash = ContentHash("abababababababababababababababab".to_string());
        let seg = SegmentId(42);

        // First time: no existing segment for this hash
        let existing = catalog.lookup_content(&hash);
        assert!(existing.is_none());

        // Register content mapping
        catalog.register_content(hash.clone(), seg).unwrap();

        // Now should find it
        let found = catalog.lookup_content(&hash);
        assert_eq!(found, Some(seg));
    }

    // ── DefaultPolicyEvaluator ────────────────────────────────────────

    #[test]
    fn default_evaluator_returns_policy_compression() {
        let eval = DefaultPolicyEvaluator;
        let policy = Policy {
            compression: CompressionPolicy::LZ4 { level: 1 },
            ..Policy::default()
        };
        let result = eval.evaluate_compression(&policy, b"sample").unwrap();
        match result {
            CompressionPolicy::LZ4 { .. } => {}
            other => panic!("expected LZ4, got: {other:?}"),
        }
    }

    #[test]
    fn default_evaluator_returns_policy_dedupe() {
        let eval = DefaultPolicyEvaluator;
        let policy = Policy {
            dedupe: true,
            ..Policy::default()
        };
        assert!(eval.evaluate_dedup(&policy).unwrap());

        let policy = Policy {
            dedupe: false,
            ..Policy::default()
        };
        assert!(!eval.evaluate_dedup(&policy).unwrap());
    }

    #[test]
    fn default_evaluator_returns_policy_encryption() {
        let eval = DefaultPolicyEvaluator;
        let policy = Policy {
            encryption: EncryptionPolicy::Disabled,
            ..Policy::default()
        };
        let result = eval.evaluate_encryption(&policy).unwrap();
        assert_eq!(result, EncryptionPolicy::Disabled);
    }

    // ── NullKeyring ───────────────────────────────────────────────────

    #[test]
    fn null_keyring_derive_returns_zeroes() {
        let keyring = NullKeyring;
        let key = keyring
            .derive_key(CapsuleId(uuid::Uuid::nil()), SegmentId(0))
            .unwrap();
        assert_eq!(key, [0u8; 32]);
    }

    #[test]
    fn null_keyring_rotate_succeeds() {
        let mut keyring = NullKeyring;
        keyring
            .rotate_key(CapsuleId(uuid::Uuid::nil()))
            .expect("rotate should succeed");
    }

    // ── KeyManagerKeyring ─────────────────────────────────────────────

    #[test]
    fn key_manager_keyring_derive_returns_key() {
        let keyring = KeyManagerKeyring::default();
        let key = keyring
            .derive_key(CapsuleId(uuid::Uuid::new_v4()), SegmentId(0))
            .unwrap();
        // Should return a non-zero key (from key manager)
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn key_manager_keyring_rotate() {
        let mut keyring = KeyManagerKeyring::default();
        keyring
            .rotate_key(CapsuleId(uuid::Uuid::new_v4()))
            .expect("rotate should succeed");
    }

    // ── PipelineBuilder ───────────────────────────────────────────────

    #[test]
    fn pipeline_builder_default_builds() {
        let pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        assert_eq!(pipeline.stats().total_segments, 0);
    }

    #[test]
    fn pipeline_builder_with_components() {
        let pipeline = PipelineBuilder::new()
            .with_compressor(Lz4ZstdCompressor)
            .with_deduper(Blake3Deduper::default())
            .with_encryptor(NoopEncryptor)
            .with_storage(InMemoryBackend::default())
            .with_evaluator(DefaultPolicyEvaluator)
            .with_keyring(NullKeyring)
            .with_catalog(InMemoryCatalog::new())
            .build();
        assert_eq!(pipeline.stats().total_segments, 0);
    }

    // ── Pipeline write + read round-trip ──────────────────────────────

    #[tokio::test]
    async fn pipeline_write_read_roundtrip() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let data = b"Hello, Pipeline roundtrip!";
        let policy = Policy::default();

        let id = pipeline.write_capsule(data, &policy).await.unwrap();
        let output = pipeline.read_capsule(id).await.unwrap();
        assert_eq!(output, data);
    }

    #[tokio::test]
    async fn pipeline_write_multiple_capsules() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = Policy::default();

        let id1 = pipeline
            .write_capsule(b"capsule-one", &policy)
            .await
            .unwrap();
        let id2 = pipeline
            .write_capsule(b"capsule-two", &policy)
            .await
            .unwrap();
        assert_ne!(id1, id2);

        let data1 = pipeline.read_capsule(id1).await.unwrap();
        let data2 = pipeline.read_capsule(id2).await.unwrap();
        assert_eq!(data1, b"capsule-one");
        assert_eq!(data2, b"capsule-two");
    }

    #[tokio::test]
    async fn pipeline_read_nonexistent_capsule() {
        let pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let fake_id = CapsuleId(uuid::Uuid::new_v4());
        let result = pipeline.read_capsule(fake_id).await;
        assert!(result.is_err());
    }

    // ── Pipeline delete ───────────────────────────────────────────────

    #[tokio::test]
    async fn pipeline_delete_capsule() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let data = b"delete me";
        let policy = Policy::default();

        let id = pipeline.write_capsule(data, &policy).await.unwrap();
        assert!(pipeline.read_capsule(id).await.is_ok());

        pipeline.delete_capsule(id).await.unwrap();
        assert!(pipeline.read_capsule(id).await.is_err());
    }

    // ── Pipeline dedup stats ──────────────────────────────────────────

    #[tokio::test]
    async fn pipeline_dedup_stats_after_writes() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = Policy {
            dedupe: true,
            ..Policy::default()
        };

        pipeline
            .write_capsule(b"dedup-data", &policy)
            .await
            .unwrap();
        let stats = pipeline.stats();
        assert!(stats.total_segments > 0);
    }

    // ── Pipeline with compression ─────────────────────────────────────

    #[tokio::test]
    async fn pipeline_compression_roundtrip() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = Policy {
            compression: CompressionPolicy::LZ4 { level: 1 },
            ..Policy::default()
        };
        let data = b"AAAA".repeat(1000); // highly compressible

        let id = pipeline.write_capsule(&data, &policy).await.unwrap();
        let output = pipeline.read_capsule(id).await.unwrap();
        assert_eq!(output, data);
    }
}

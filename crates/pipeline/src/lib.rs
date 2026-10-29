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
use dedup::{verify_content_hash, Blake3Deduper, VerifyOutcome};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use storage::{InMemoryBackend, NvramBackend};
use tracing::instrument;

use encryption::{
    compute_mac, derive_tweak_from_hash, encrypt_segment, keymanager::MASTER_KEY_SIZE,
    xts::MIN_SECTOR_SIZE, EncryptionMetadata, KeyManager,
};
/// Minimal encryptor that performs no-op transformations.
#[derive(Default, Clone)]
pub struct NoopEncryptor;

impl Encryptor for NoopEncryptor {
    fn encrypt(
        &self,
        data: Cow<'_, [u8]>,
        policy: &EncryptionPolicy,
        _segment: SegmentId,
    ) -> Result<(Vec<u8>, EncryptionSummary)> {
        // Handing back plaintext for a policy that asks for encryption would
        // store it unprotected and label it encrypted (#84).
        if policy.is_enabled() {
            return Err(anyhow!(
                "encryption requested but no key is configured (set SPACE_MASTER_KEY)"
            ));
        }
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
        let ciphertext_len = match ctx.ciphertext_len {
            Some(len) => len,
            None => u32::try_from(data.len()).context("ciphertext too large")?,
        };
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
        let len = u32::try_from(data.len()).context("ciphertext too large")?;
        let (metadata, key_pair) = self.build_mac_metadata(ctx, len, None, None)?;
        let tag = encryption::compute_mac(data, &metadata, key_pair.key1(), key_pair.key2())
            .context("failed to compute MAC")?;
        Ok(tag.to_vec())
    }

    fn verify_mac(&self, data: &[u8], _segment: SegmentId, ctx: &DecryptContext) -> Result<()> {
        let len = u32::try_from(data.len()).context("ciphertext too large")?;
        let (metadata, key_pair) = self.build_mac_metadata(ctx, len, ctx.integrity_tag, None)?;
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

/// Dedup key domain for a segment. Encrypted and plaintext segments must
/// never share storage, so encryption is part of the domain next to the
/// compression algorithm (#83). Unencrypted keys are unchanged.
fn dedup_domain<'a>(compression_algo: &'a str, encryption: &EncryptionPolicy) -> Cow<'a, str> {
    if encryption.is_enabled() {
        Cow::Owned(format!("{compression_algo}+xts-aes-256"))
    } else {
        Cow::Borrowed(compression_algo)
    }
}

/// Turn stored segment bytes back into plaintext. The streaming and range
/// read paths share this so both apply the same integrity checks.
fn decode_segment<C: Compressor, E: Encryptor>(
    compressor: &C,
    encryptor: &E,
    capsule_encryption: &EncryptionPolicy,
    seg_id: SegmentId,
    metadata: &Segment,
    raw: Vec<u8>,
) -> Result<Vec<u8>> {
    let stored = if metadata.encrypted {
        let ctx = DecryptContext::from_segment(metadata);
        let mut plain = encryptor
            .decrypt(&raw, capsule_encryption, seg_id, &ctx)
            .map_err(|e| anyhow!("Decryption failed for segment {:?}: {}", seg_id, e))?;
        // Segments shorter than one XTS block are zero-padded on write.
        if !metadata.compressed {
            if let Some(plain_len) = metadata.plain_len {
                plain.truncate(plain_len as usize);
            }
        }
        plain
    } else {
        // The segment sidecar is not authenticated. A capsule that requires
        // encryption must never be served from a segment marked plaintext,
        // and plaintext segments are checked against their content hash (#95).
        if capsule_encryption.is_enabled() {
            return Err(anyhow!(
                "segment {:?} is stored unencrypted but its capsule requires encryption",
                seg_id
            ));
        }
        if let Some(expected) = &metadata.content_hash {
            if verify_content_hash(expected, &raw, &metadata.compression_algo)
                == VerifyOutcome::Mismatched
            {
                return Err(anyhow!("content hash mismatch for segment {:?}", seg_id));
            }
        }
        raw
    };

    if metadata.compressed {
        compressor
            .decompress(&stored, metadata.compression_algo.as_str())
            .map_err(|e| anyhow!("Decompression failed for segment {:?}: {}", seg_id, e))
    } else {
        Ok(stored)
    }
}

/// An open capsule write: `begin_write`, then `write_chunk` for each chunk,
/// then `finish_write` or `abort_write`. Every chunk but the last is exactly
/// `chunk_size()` bytes, so a capsule is cut into the same segments however
/// it is written, and dedups the same way.
pub struct CapsuleWrite {
    capsule_id: CapsuleId,
    policy: Policy,
    chunk_size: usize,
    compression_policy: Option<CompressionPolicy>,
    encryption_policy: EncryptionPolicy,
    dedupe: bool,
    segments: Vec<SegmentId>,
    stats: DedupStats,
    size: u64,
    ended: bool,
}

impl CapsuleWrite {
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
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
    /// Writes begun and not yet finished or aborted.
    open_writes: usize,
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
            open_writes: 0,
        }
    }

    #[instrument(skip_all)]
    pub async fn write_capsule(&mut self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        let mut write = self.begin_write(policy)?;
        for chunk in data.chunks(write.chunk_size()) {
            if let Err(err) = self.write_chunk(&mut write, chunk).await {
                self.abort_after_error(write).await;
                return Err(err);
            }
        }
        self.finish_write(write).await
    }

    /// Start a write that takes the capsule one chunk at a time, so the
    /// caller never holds more than one segment of it (#99).
    pub fn begin_write(&mut self, policy: &Policy) -> Result<CapsuleWrite> {
        policy.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
        let write = CapsuleWrite {
            capsule_id: CapsuleId::new(),
            policy: policy.clone(),
            // `Policy::validate` keeps this between 1 byte and 16 MiB.
            chunk_size: policy.layout.strategy.default_segment_size(),
            compression_policy: None,
            encryption_policy: self.evaluator.evaluate_encryption(policy)?,
            dedupe: self.evaluator.evaluate_dedup(policy)?,
            segments: Vec::new(),
            stats: DedupStats::new(),
            size: 0,
            ended: false,
        };
        self.open_writes += 1;
        Ok(write)
    }

    /// Store the next chunk of an open write as one segment. Every chunk but
    /// the last must be exactly `chunk_size()` bytes. Empty chunks are ignored.
    pub async fn write_chunk(&mut self, write: &mut CapsuleWrite, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        if write.ended || chunk.len() > write.chunk_size {
            return Err(anyhow!(
                "chunks must be {} bytes; only the last may be shorter",
                write.chunk_size
            ));
        }
        write.ended = chunk.len() < write.chunk_size;
        let compression_policy = match &write.compression_policy {
            Some(policy) => policy.clone(),
            None => {
                let policy = self
                    .evaluator
                    .evaluate_compression(&write.policy, &chunk[..chunk.len().min(1024)])?;
                write.compression_policy = Some(policy.clone());
                policy
            }
        };

        let (view, summary) = self.compressor.compress(chunk, &compression_policy)?;
        // XTS needs one full block. A compressed view shorter than that is
        // stored uncompressed instead, so `plain_len` is the exact length
        // to strip the block padding back to on read (#94).
        let (view, summary) = if write.encryption_policy.is_enabled()
            && summary.compressed
            && view.len() < MIN_SECTOR_SIZE
        {
            self.compressor.compress(chunk, &CompressionPolicy::None)?
        } else {
            (view, summary)
        };
        // Domain-separate the dedup key by compression algorithm and
        // encryption so segments with identical stored bytes under
        // different treatments cannot collide.
        let hash = self.deduper.hash_content_with_algo(
            view.as_ref(),
            &dedup_domain(summary.algorithm.as_str(), &write.encryption_policy),
        );

        // With dedupe off, never reuse a segment, and never register one
        // for later writes to attach to (#87).
        let existing = if write.dedupe {
            self.catalog.lookup_content(&hash)
        } else {
            None
        };
        if let Some(existing) = existing {
            let mut metadata = self.storage.metadata(existing).await?;
            metadata.ref_count += 1;
            metadata.deduplicated = metadata.ref_count > 1;
            let mut txn = self.storage.begin_txn().await?;
            txn.set_segment_metadata(existing, metadata).await?;
            txn.commit().await?;
            self.deduper.update_stats(summary.output_size as u64, true);
            self.stats.record(summary.output_size as u64, true);
            write.stats.record(summary.output_size as u64, true);
            write.segments.push(existing);
        } else {
            let mut txn = self.storage.begin_txn().await?;
            let seg_id = self.catalog.allocate_segment()?;

            let (payload, encryption_summary) = if write.encryption_policy.is_enabled() {
                let _key = self
                    .keyring
                    .as_ref()
                    .map(|keyring| keyring.derive_key(write.capsule_id, seg_id))
                    .transpose()?;
                let plaintext = if view.len() < MIN_SECTOR_SIZE {
                    let mut padded = view.to_vec();
                    padded.resize(MIN_SECTOR_SIZE, 0);
                    Cow::Owned(padded)
                } else {
                    Cow::Borrowed(view.as_ref())
                };
                let (encrypted, summary) =
                    self.encryptor
                        .encrypt(plaintext, &write.encryption_policy, seg_id)?;
                (encrypted, summary)
            } else {
                (view.into_owned(), EncryptionSummary::new("none"))
            };

            txn.append(seg_id, &payload).await?;
            let metadata = Segment {
                id: seg_id,
                offset: 0,
                len: u32::try_from(payload.len()).context("segment too large")?,
                plain_len: Some(u32::try_from(summary.original_size).context("segment too large")?),
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
                encrypted: encryption_summary.is_encrypted(),
                pq_ciphertext: None,
                pq_nonce: None,
            };
            txn.set_segment_metadata(seg_id, metadata).await?;
            txn.commit().await?;
            // Recorded before anything else can fail, so an abort releases it.
            write.segments.push(seg_id);

            if write.dedupe {
                self.catalog.register_content(hash.clone(), seg_id)?;
                self.deduper.register_content(hash, seg_id)?;
            }
            self.deduper.update_stats(summary.output_size as u64, false);
            self.stats.record(summary.output_size as u64, false);
            write.stats.record(summary.output_size as u64, false);
        }

        write.size += chunk.len() as u64;
        Ok(())
    }

    /// Record the capsule. If that fails, the write's segments are released.
    pub async fn finish_write(&mut self, write: CapsuleWrite) -> Result<CapsuleId> {
        if let Err(err) = self.catalog.create_capsule(
            write.capsule_id,
            write.size,
            &write.policy,
            write.segments.clone(),
            &write.stats,
        ) {
            self.abort_after_error(write).await;
            return Err(err);
        }
        self.open_writes = self.open_writes.saturating_sub(1);
        Ok(write.capsule_id)
    }

    /// Give up on an open write and release the segments it holds.
    pub async fn abort_write(&mut self, write: CapsuleWrite) -> Result<()> {
        self.open_writes = self.open_writes.saturating_sub(1);
        self.release_segments(&write.segments).await
    }

    async fn abort_after_error(&mut self, write: CapsuleWrite) {
        if let Err(err) = self.abort_write(write).await {
            tracing::warn!("failed to release the segments of a failed write: {err:#}");
        }
    }

    pub fn stats(&self) -> DedupStats {
        self.stats.clone()
    }

    pub fn catalog(&self) -> &R {
        &self.catalog
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
        self.read_range_stream(id, 0, u64::MAX).await
    }

    /// Stream up to `len` bytes of a capsule from `offset`, one segment at a
    /// time. Segments before `offset` are skipped without being read.
    pub async fn read_range_stream(
        &self,
        id: CapsuleId,
        offset: u64,
        len: u64,
    ) -> Result<DataStream>
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
        let end = offset.saturating_add(len).min(capsule.size);
        let storage_stream = self.storage.clone();
        let encryptor_stream = self.encryptor.clone();
        let compressor_stream = self.compressor.clone();
        let encryption_policy = capsule.policy.encryption.clone();

        let stream = try_stream! {
            let mut cursor = 0u64;
            for seg_id in capsule.segments {
                if cursor >= end {
                    break;
                }
                let metadata = storage_stream.metadata(seg_id).await
                    .map_err(|e| anyhow!("Failed to fetch metadata for segment {:?}: {}", seg_id, e))?;
                let seg_len_hint = metadata
                    .plain_len
                    .map(u64::from)
                    .or_else(|| (!metadata.compressed).then_some(metadata.len as u64));
                if let Some(seg_len) = seg_len_hint {
                    if cursor + seg_len <= offset {
                        cursor += seg_len;
                        continue;
                    }
                }

                let raw = storage_stream.read(seg_id).await
                    .map_err(|e| anyhow!("Failed to read segment {:?}: {}", seg_id, e))?;

                let plain = decode_segment(
                    &compressor_stream,
                    &encryptor_stream,
                    &encryption_policy,
                    seg_id,
                    &metadata,
                    raw,
                )?;

                let seg_end = cursor + plain.len() as u64;
                if seg_end > offset {
                    let from = offset.saturating_sub(cursor) as usize;
                    let to = (end.min(seg_end) - cursor) as usize;
                    yield Bytes::from(plain).slice(from..to);
                }
                cursor = seg_end;
            }
        };

        let out: DataStream = Box::pin(stream);

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
            let decompressed = decode_segment(
                &self.compressor,
                &self.encryptor,
                &capsule.policy.encryption,
                *seg_id,
                &metadata,
                raw,
            )?;

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
        self.release_segments(&capsule.segments).await?;
        self.catalog.delete_capsule(id)?;
        Ok(())
    }

    /// Drop one reference to each segment. The last reference deletes the
    /// segment and unregisters its hash.
    async fn release_segments(&mut self, segments: &[SegmentId]) -> Result<()> {
        for seg_id in segments {
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
        Ok(())
    }

    pub async fn garbage_collect(&mut self) -> Result<usize> {
        // An open write holds segments that no capsule references yet.
        if self.open_writes > 0 {
            return Err(anyhow!(
                "garbage collection can't run while a write is open"
            ));
        }
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

    // ── Encryption integrity ──────────────────────────────────────────

    type XtsInMemoryPipeline = Pipeline<
        Lz4ZstdCompressor,
        Blake3Deduper,
        XtsEncryptor,
        InMemoryBackend,
        DefaultPolicyEvaluator,
        KeyManagerKeyring,
        InMemoryCatalog,
    >;

    fn xts_pipeline() -> XtsInMemoryPipeline {
        let km = Arc::new(Mutex::new(KeyManager::new([7u8; MASTER_KEY_SIZE])));
        Pipeline::new(
            Lz4ZstdCompressor,
            Blake3Deduper::default(),
            XtsEncryptor::new(Arc::clone(&km)),
            InMemoryBackend::new(),
            DefaultPolicyEvaluator,
            Some(KeyManagerKeyring::new(km)),
            InMemoryCatalog::default(),
        )
    }

    fn encrypted_policy() -> Policy {
        Policy {
            encryption: EncryptionPolicy::XtsAes256 { key_version: None },
            ..Policy::default()
        }
    }

    fn with_segment_size(policy: Policy, segment_size: usize) -> Policy {
        let mut policy = policy;
        policy.layout.strategy = common::LayoutStrategy::Fixed { segment_size };
        policy
    }

    #[tokio::test]
    async fn encrypted_and_plaintext_writes_never_share_segments() {
        let mut p = xts_pipeline();
        let data = b"ALPHA-SECRET-PAYLOAD-0123456789";

        let plain = p.write_capsule(data, &Policy::default()).await.unwrap();
        let enc = p.write_capsule(data, &encrypted_policy()).await.unwrap();
        let plain_segs = p.catalog.lookup_capsule(plain).unwrap().segments;
        let enc_segs = p.catalog.lookup_capsule(enc).unwrap().segments;
        assert_ne!(plain_segs, enc_segs);
        for seg in &enc_segs {
            assert!(p.storage.metadata(*seg).await.unwrap().encrypted);
        }

        // The other order: a plaintext write must not reuse an encrypted segment.
        let other = b"BRAVO-SECRET-PAYLOAD-0123456789";
        let enc = p.write_capsule(other, &encrypted_policy()).await.unwrap();
        let plain = p.write_capsule(other, &Policy::default()).await.unwrap();
        assert_ne!(
            p.catalog.lookup_capsule(enc).unwrap().segments,
            p.catalog.lookup_capsule(plain).unwrap().segments
        );
        assert_eq!(p.read_capsule(enc).await.unwrap(), other);
        assert_eq!(p.read_capsule(plain).await.unwrap(), other);
    }

    #[tokio::test]
    async fn encrypted_write_without_key_fails_and_stores_nothing() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let result = p
            .write_capsule(b"BRAVO-SECRET-PAYLOAD", &encrypted_policy())
            .await;
        assert!(result.is_err());
        assert!(p.storage.segment_ids().await.unwrap().is_empty());
        assert!(p.catalog.capsules().is_empty());
    }

    #[tokio::test]
    async fn encrypted_writes_handle_segments_shorter_than_a_block() {
        let mut p = xts_pipeline();
        let policy = with_segment_size(encrypted_policy(), 64);

        for len in [1usize, 5, 15, 16, 17, 64, 64 * 2 + 5] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
            let id = p
                .write_capsule(&data, &policy)
                .await
                .unwrap_or_else(|e| panic!("write of {len} bytes failed: {e}"));
            assert_eq!(p.read_capsule(id).await.unwrap(), data, "len {len}");
            assert_eq!(p.read_range(id, 0, len).await.unwrap(), data, "len {len}");
        }

        // A short tail read on its own.
        let data: Vec<u8> = (0..64 * 2 + 5).map(|i| (i * 13 + 1) as u8).collect();
        let id = p.write_capsule(&data, &policy).await.unwrap();
        assert_eq!(p.read_range(id, 128, 5).await.unwrap(), &data[128..]);
    }

    /// Run-length "compressor" whose output is 5 bytes for any uniform
    /// input, far below one XTS block. The built-in LZ4 and Zstd frames are
    /// never that short.
    #[derive(Clone, Default)]
    struct TinyRle;

    use common::traits::CompressionSummary;

    impl Compressor for TinyRle {
        fn compress<'a>(
            &'a self,
            data: &'a [u8],
            policy: &CompressionPolicy,
        ) -> Result<(Cow<'a, [u8]>, CompressionSummary)> {
            let uniform = data.windows(2).all(|w| w[0] == w[1]);
            if matches!(policy, CompressionPolicy::None) || data.len() <= 5 || !uniform {
                let mut summary = CompressionSummary::new(data.len(), data.len(), "identity");
                summary.compressed = false;
                return Ok((Cow::Borrowed(data), summary));
            }
            let mut out = vec![data[0]];
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            let summary = CompressionSummary::new(data.len(), out.len(), "rle");
            Ok((Cow::Owned(out), summary))
        }

        fn decompress(&self, data: &[u8], algorithm: &str) -> Result<Vec<u8>> {
            match (algorithm, data) {
                ("identity", _) => Ok(data.to_vec()),
                ("rle", [byte, a, b, c, d]) => {
                    Ok(vec![*byte; u32::from_le_bytes([*a, *b, *c, *d]) as usize])
                }
                _ => Err(anyhow!("bad {algorithm} frame of {} bytes", data.len())),
            }
        }
    }

    #[tokio::test]
    async fn encrypted_segments_that_compress_below_a_block_roundtrip() {
        let km = Arc::new(Mutex::new(KeyManager::new([7u8; MASTER_KEY_SIZE])));
        let mut p = Pipeline::new(
            TinyRle,
            Blake3Deduper::default(),
            XtsEncryptor::new(Arc::clone(&km)),
            InMemoryBackend::new(),
            DefaultPolicyEvaluator,
            Some(KeyManagerKeyring::new(km)),
            InMemoryCatalog::default(),
        );
        let policy = Policy {
            compression: CompressionPolicy::LZ4 { level: 1 },
            ..with_segment_size(encrypted_policy(), 256)
        };
        let data = vec![0xAAu8; 256 * 2 + 3];
        let id = p.write_capsule(&data, &policy).await.unwrap();
        assert_eq!(p.read_capsule(id).await.unwrap(), data);

        // Plaintext writes still use the short frame.
        let plain = Policy {
            compression: CompressionPolicy::LZ4 { level: 1 },
            ..with_segment_size(Policy::default(), 256)
        };
        let id = p.write_capsule(&data, &plain).await.unwrap();
        let seg = p.catalog.lookup_capsule(id).unwrap().segments[0];
        assert!(p.storage.metadata(seg).await.unwrap().compressed);
        assert_eq!(p.read_capsule(id).await.unwrap(), data);
    }

    #[tokio::test]
    async fn read_rejects_corrupted_plaintext_segment() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = Policy {
            compression: CompressionPolicy::None,
            ..Policy::default()
        };
        let data: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let id = p.write_capsule(&data, &policy).await.unwrap();
        let seg = p.catalog.lookup_capsule(id).unwrap().segments[0];

        let mut raw = p.storage.read(seg).await.unwrap();
        raw[2048] ^= 0x01;
        let mut txn = p.storage.begin_txn().await.unwrap();
        txn.append(seg, &raw).await.unwrap();
        txn.commit().await.unwrap();

        assert!(p.read_capsule(id).await.is_err());
        assert!(p.read_range(id, 0, 16).await.is_err());
    }

    #[tokio::test]
    async fn read_rejects_plaintext_flag_on_encrypted_capsule() {
        let mut p = xts_pipeline();
        let data = b"CHARLIE-SECRET-PAYLOAD-0123456789".repeat(10);
        let id = p.write_capsule(&data, &encrypted_policy()).await.unwrap();
        let seg = p.catalog.lookup_capsule(id).unwrap().segments[0];

        let mut meta = p.storage.metadata(seg).await.unwrap();
        meta.encrypted = false;
        meta.compressed = false;
        let mut txn = p.storage.begin_txn().await.unwrap();
        txn.set_segment_metadata(seg, meta).await.unwrap();
        txn.commit().await.unwrap();

        assert!(p.read_capsule(id).await.is_err());
        assert!(p.read_range(id, 0, 16).await.is_err());
    }

    // ── Policy enforcement ────────────────────────────────────────────

    #[tokio::test]
    async fn dedupe_off_writes_get_their_own_segments() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let nodedup = Policy {
            dedupe: false,
            ..Policy::default()
        };
        let data = [42u8; 100_000];

        let w = p.write_capsule(&data, &Policy::default()).await.unwrap();
        let x = p.write_capsule(&data, &nodedup).await.unwrap();
        let y = p.write_capsule(&data, &nodedup).await.unwrap();
        let w_segs = p.catalog.lookup_capsule(w).unwrap().segments;
        let x_segs = p.catalog.lookup_capsule(x).unwrap().segments;
        let y_segs = p.catalog.lookup_capsule(y).unwrap().segments;
        assert_ne!(
            x_segs, w_segs,
            "dedupe-off write reused an existing segment"
        );
        assert_ne!(x_segs, y_segs);

        // A later dedupe-on write attaches to the dedupe-on segment, never to
        // an opted-out one.
        let z = p.write_capsule(&data, &Policy::default()).await.unwrap();
        assert_eq!(p.catalog.lookup_capsule(z).unwrap().segments, w_segs);
        for seg in x_segs.iter().chain(&y_segs) {
            assert_eq!(p.storage.metadata(*seg).await.unwrap().ref_count, 1);
        }

        for id in [w, x, y, z] {
            assert_eq!(p.read_capsule(id).await.unwrap(), data);
        }
    }

    #[tokio::test]
    async fn write_rejects_out_of_range_segment_size() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        for segment_size in [0, common::MAX_SEGMENT_SIZE + 1] {
            let mut policy = Policy::default();
            policy.layout.strategy = common::LayoutStrategy::Fixed { segment_size };
            assert!(p.write_capsule(b"data", &policy).await.is_err());
        }
        assert!(p.storage.segment_ids().await.unwrap().is_empty());
    }

    fn small_segments(segment_size: usize) -> Policy {
        let mut policy = Policy::default();
        policy.layout.strategy = common::LayoutStrategy::Fixed { segment_size };
        policy
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = (seed << 1) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[tokio::test]
    async fn chunked_writes_cut_the_same_segments_as_write_capsule() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = small_segments(1000);
        let data = noise(3500, 1);

        let whole = p.write_capsule(&data, &policy).await.unwrap();
        let mut write = p.begin_write(&policy).unwrap();
        assert_eq!(write.chunk_size(), 1000);
        for chunk in data.chunks(1000) {
            p.write_chunk(&mut write, chunk).await.unwrap();
        }
        p.write_chunk(&mut write, &[]).await.unwrap();
        let chunked = p.finish_write(write).await.unwrap();

        let whole_segs = p.catalog.lookup_capsule(whole).unwrap().segments;
        assert_eq!(whole_segs.len(), 4);
        assert_eq!(
            p.catalog.lookup_capsule(chunked).unwrap().segments,
            whole_segs
        );
        assert_eq!(p.catalog.lookup_capsule(chunked).unwrap().size, 3500);
        assert_eq!(p.read_capsule(chunked).await.unwrap(), data);
    }

    #[tokio::test]
    async fn only_the_last_chunk_may_be_short() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = small_segments(1000);

        let mut write = p.begin_write(&policy).unwrap();
        assert!(p.write_chunk(&mut write, &[1; 1001]).await.is_err());
        p.write_chunk(&mut write, &[1; 500]).await.unwrap();
        assert!(p.write_chunk(&mut write, &[2; 1000]).await.is_err());
        p.abort_write(write).await.unwrap();
    }

    #[tokio::test]
    async fn aborting_a_write_releases_its_segments() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = small_segments(1000);
        let kept = noise(1000, 2);
        let existing = p.write_capsule(&kept, &policy).await.unwrap();
        let shared = p.catalog.lookup_capsule(existing).unwrap().segments[0];

        let mut write = p.begin_write(&policy).unwrap();
        p.write_chunk(&mut write, &kept).await.unwrap();
        p.write_chunk(&mut write, &noise(1000, 3)).await.unwrap();
        assert_eq!(p.storage.metadata(shared).await.unwrap().ref_count, 2);
        assert_eq!(p.storage.segment_ids().await.unwrap().len(), 2);

        p.abort_write(write).await.unwrap();
        assert_eq!(p.storage.metadata(shared).await.unwrap().ref_count, 1);
        assert_eq!(p.storage.segment_ids().await.unwrap(), vec![shared]);
        assert_eq!(p.catalog.content_entries().len(), 1);
        assert_eq!(p.catalog.capsules().len(), 1);
        assert_eq!(p.read_capsule(existing).await.unwrap(), kept);
    }

    #[tokio::test]
    async fn garbage_collection_waits_for_open_writes() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = small_segments(1000);

        let mut write = p.begin_write(&policy).unwrap();
        p.write_chunk(&mut write, &noise(1000, 4)).await.unwrap();
        assert!(p.garbage_collect().await.is_err());
        let id = p.finish_write(write).await.unwrap();

        assert_eq!(p.garbage_collect().await.unwrap(), 0);
        assert_eq!(p.read_capsule(id).await.unwrap(), noise(1000, 4));
    }

    #[tokio::test]
    async fn range_streams_match_read_range() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let data = noise(3500, 5);
        let id = p.write_capsule(&data, &small_segments(1000)).await.unwrap();

        for (offset, len) in [
            (0, 3500),
            (0, 1),
            (999, 2),
            (1000, 1000),
            (1500, 1700),
            (3499, 10),
            (3500, 1),
            (4000, 5),
            (0, 0),
            (10, u64::MAX),
        ] {
            let mut stream = p.read_range_stream(id, offset, len).await.unwrap();
            let mut got = Vec::new();
            while let Some(chunk) = stream.next().await {
                got.extend_from_slice(&chunk.unwrap());
            }
            let expected = p
                .read_range(id, offset, len.min(usize::MAX as u64) as usize)
                .await
                .unwrap();
            assert_eq!(got, expected, "offset {offset} len {len}");
        }
    }
}

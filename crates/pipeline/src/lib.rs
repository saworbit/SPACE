use std::borrow::Cow;

use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use bytes::Bytes;
use common::format::{CapsuleManifest, JournalRecord, ManifestEntry};
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
use std::collections::HashMap;
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

    /// Reads only call `decrypt` for segments stored encrypted, and without a
    /// key there is no way to recover their plaintext. Handing back the
    /// stored bytes would serve ciphertext as the capsule's contents (#155).
    fn decrypt(
        &self,
        _data: &[u8],
        _policy: &EncryptionPolicy,
        _segment: SegmentId,
        _ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        Err(no_key())
    }

    /// A MAC needs the key too. An empty tag, or a check that always
    /// passes, would let a caller treat unverified bytes as authentic
    /// (#176).
    fn compute_mac(
        &self,
        _data: &[u8],
        _segment: SegmentId,
        _ctx: &DecryptContext,
    ) -> Result<Vec<u8>> {
        Err(no_key())
    }

    fn verify_mac(&self, _data: &[u8], _segment: SegmentId, _ctx: &DecryptContext) -> Result<()> {
        Err(no_key())
    }
}

/// What [`NoopEncryptor`] says for anything that needs a key.
fn no_key() -> anyhow::Error {
    anyhow!("it is encrypted and no key is configured (set SPACE_MASTER_KEY)")
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

    /// The key manager this keyring derives keys from.
    pub fn manager(&self) -> Arc<Mutex<KeyManager>> {
        Arc::clone(&self.manager)
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

    /// Capsule records only: this catalog keeps no names, so name records
    /// are accepted and change nothing.
    fn commit(&self, records: Vec<JournalRecord>) -> Result<Vec<Capsule>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for record in &records {
            match record {
                JournalRecord::Drop(id) if !inner.capsules.contains_key(id) => {
                    return Err(anyhow!("capsule {:?} not found", id));
                }
                JournalRecord::Batch(_) => return Err(anyhow!("a batch can't hold another batch")),
                _ => {}
            }
        }
        let mut dropped = Vec::new();
        for record in records {
            match record {
                JournalRecord::Capsule(manifest) => {
                    inner
                        .capsules
                        .insert(manifest.capsule_id, manifest.to_capsule());
                }
                JournalRecord::Drop(id) => dropped.extend(inner.capsules.remove(&id)),
                _ => {}
            }
        }
        Ok(dropped)
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
/// A content hash as the raw bytes a manifest entry holds; all zero if it
/// isn't a 32-byte hex hash.
fn hash_bytes(hash: &ContentHash) -> [u8; 32] {
    hex::decode(hash.as_str())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .unwrap_or([0u8; 32])
}

/// The manifest entry for a dedup hit on `segment`, which holds `plain_len`
/// logical bytes.
fn manifest_entry(
    segment: SegmentId,
    metadata: &Segment,
    plain_len: usize,
) -> Result<ManifestEntry> {
    Ok(ManifestEntry {
        segment_id: segment,
        plain_len: u32::try_from(plain_len).context("segment too large")?,
        content_hash: metadata
            .content_hash
            .as_ref()
            .map(hash_bytes)
            .unwrap_or([0u8; 32]),
    })
}

fn dedup_domain<'a>(compression_algo: &'a str, encryption: &EncryptionPolicy) -> Cow<'a, str> {
    if encryption.is_enabled() {
        Cow::Owned(format!("{compression_algo}+xts-aes-256"))
    } else {
        Cow::Borrowed(compression_algo)
    }
}

/// Turn stored segment bytes back into plaintext. The streaming and range
/// read paths share this so both apply the same integrity checks.
/// Every chunk of `stream`, in one buffer.
async fn collect(mut stream: DataStream) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        output.extend_from_slice(&chunk?);
    }
    Ok(output)
}

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
    /// One per segment, in logical byte order; becomes the manifest.
    entries: Vec<ManifestEntry>,
    /// BLAKE3 over the logical bytes so far.
    digest: blake3::Hasher,
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
    /// Segments that no committed capsule may reference but that must not
    /// be collected: those an open write holds, and those of a capsule a
    /// commit dropped until [`Pipeline::release`] is called for it. Counts,
    /// since a segment can be held more than once.
    pins: HashMap<SegmentId, u32>,
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
            pins: HashMap::new(),
        }
    }

    fn pin(&mut self, segments: &[SegmentId]) {
        for seg in segments {
            *self.pins.entry(*seg).or_insert(0) += 1;
        }
    }

    fn unpin(&mut self, segments: &[SegmentId]) {
        for seg in segments {
            if let Some(count) = self.pins.get_mut(seg) {
                *count -= 1;
                if *count == 0 {
                    self.pins.remove(seg);
                }
            }
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
            entries: Vec::new(),
            digest: blake3::Hasher::new(),
            stats: DedupStats::new(),
            size: 0,
            ended: false,
        };
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
        write.digest.update(chunk);
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
            let entry = manifest_entry(existing, &metadata, chunk.len())?;
            let mut txn = self.storage.begin_txn().await?;
            txn.set_segment_metadata(existing, metadata).await?;
            txn.commit().await?;
            self.deduper.update_stats(summary.output_size as u64, true);
            self.stats.record(summary.output_size as u64, true);
            write.stats.record(summary.output_size as u64, true);
            write.segments.push(existing);
            self.pin(&[existing]);
            write.entries.push(entry);
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
            self.pin(&[seg_id]);
            write.entries.push(ManifestEntry {
                segment_id: seg_id,
                plain_len: u32::try_from(summary.original_size).context("segment too large")?,
                content_hash: hash_bytes(&hash),
            });

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
        let id = write.capsule_id;
        self.finish_write_with(write, Vec::new()).await?;
        Ok(id)
    }

    /// Record the capsule together with `with`, all or nothing
    /// (`docs/on-disk-format.md` §5.7): an S3 overwrite commits the capsule,
    /// its key and the drop of the capsule the key pointed at as one batch.
    /// Returns the capsules `with` dropped. Their segments stay until
    /// [`Pipeline::release`] is called for each, so a reader can finish with
    /// them first. If the commit fails, the write's segments are released.
    pub async fn finish_write_with(
        &mut self,
        write: CapsuleWrite,
        with: Vec<JournalRecord>,
    ) -> Result<Vec<Capsule>> {
        let manifest = CapsuleManifest {
            capsule_id: write.capsule_id,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            size: write.size,
            digest: *write.digest.finalize().as_bytes(),
            dedupe: write.dedupe,
            requires_encryption: write.encryption_policy.is_enabled(),
            deduped_bytes: write.stats.bytes_saved,
            policy: write.policy.clone(),
            entries: write.entries.clone(),
        };
        let mut records = Vec::with_capacity(1 + with.len());
        records.push(JournalRecord::Capsule(Box::new(manifest)));
        records.extend(with);
        match self.catalog.commit(records) {
            Ok(dropped) => {
                // The capsule references the write's segments now.
                self.unpin(&write.segments);
                self.hold(&dropped);
                Ok(dropped)
            }
            Err(err) => {
                self.abort_after_error(write).await;
                Err(err)
            }
        }
    }

    /// Commit `records`, all or nothing. Returns the capsules they dropped,
    /// whose segments stay until [`Pipeline::release`] is called for each.
    pub fn commit(&mut self, records: Vec<JournalRecord>) -> Result<Vec<Capsule>> {
        let dropped = self.catalog.commit(records)?;
        self.hold(&dropped);
        Ok(dropped)
    }

    /// Pin the segments of capsules a commit dropped until they're released.
    fn hold(&mut self, dropped: &[Capsule]) {
        for capsule in dropped {
            self.pin(&capsule.segments);
        }
    }

    /// Release a dropped capsule's references to its segments. Until then
    /// they pin the segments (`docs/on-disk-format.md` §6): each stays
    /// readable, garbage collection and compaction keep it, and it leaves
    /// the store when its last reference goes.
    pub async fn release(&mut self, capsule: &Capsule) -> Result<()> {
        self.unpin(&capsule.segments);
        self.release_segments(&capsule.segments).await
    }

    /// Give up on an open write and release the segments it holds.
    pub async fn abort_write(&mut self, write: CapsuleWrite) -> Result<()> {
        self.unpin(&write.segments);
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
    /// time. Segments before `offset` are skipped without being read. This
    /// is the one walk over a capsule's segments: every read goes through
    /// it (#190).
    ///
    /// A stream that covers the whole capsule hashes what it yields, and
    /// ends with an error instead of cleanly if that doesn't match the
    /// capsule's digest. A range can't be checked against a whole-capsule
    /// digest. Either way, segments that end before the bytes asked for are
    /// an error, not a short read.
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
        let mut digest = capsule
            .digest
            .filter(|_| offset == 0 && end == capsule.size)
            .map(|expected| (expected, blake3::Hasher::new()));

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
                    let piece = Bytes::from(plain).slice(from..to);
                    if let Some((_, hasher)) = &mut digest {
                        hasher.update(&piece);
                    }
                    yield piece;
                }
                cursor = seg_end;
            }
            if cursor < end {
                Err(anyhow!(
                    "capsule {:?}'s segments hold {} bytes, short of the {} asked for; they're damaged",
                    id,
                    cursor,
                    end
                ))?;
            }
            if let Some((expected, hasher)) = digest {
                if *hasher.finalize().as_bytes() != expected {
                    Err(anyhow!(
                        "capsule {:?} doesn't match its digest; its segments are damaged",
                        id
                    ))?;
                }
            }
        };

        let out: DataStream = Box::pin(stream);

        Ok(out)
    }

    /// The whole capsule in memory, checked against its digest
    /// ([`Pipeline::read_range_stream`]). Prefer
    /// [`Pipeline::read_capsule_stream`], which doesn't hold it all.
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
        collect(self.read_capsule_stream(id).await?).await
    }

    /// Up to `len` bytes from `offset`, in memory
    /// ([`Pipeline::read_range_stream`]).
    pub async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>>
    where
        C: Compressor + Clone + Send + Sync + 'static,
        D: Deduper + Send + Sync + 'static,
        E: Encryptor + Clone + Send + Sync + 'static,
        S: StorageBackend + Clone + Send + Sync + 'static,
        Eval: PolicyEvaluator + Send + Sync + 'static,
        K: Keyring + Send + Sync + 'static,
        R: CapsuleCatalog + Send + Sync + 'static,
    {
        collect(self.read_range_stream(id, offset, len as u64).await?).await
    }

    /// Commit the delete first, then release the capsule's references to
    /// its segments. Ref counts are in memory, so a crash in between only
    /// leaves counts that the next open recounts.
    pub async fn delete_capsule(&mut self, id: CapsuleId) -> Result<()> {
        let capsule = self.catalog.delete_capsule(id)?;
        self.release_segments(&capsule.segments).await
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

    /// Remove segments that no committed capsule references and nothing
    /// pins: leftovers of writes that failed or were interrupted. Segments
    /// an open write holds, or a dropped capsule holds until it's released,
    /// are kept, and so is every segment a capsule references, whatever its
    /// stored ref count says. Returns how many were removed.
    ///
    /// On the NVRAM log a removed segment leaves the map, and compaction
    /// reclaims its bytes. Compaction already leaves out segments with no
    /// references, so the log needs no garbage collection of its own; file
    /// backends do, since each segment is a file.
    pub async fn garbage_collect(&mut self) -> Result<usize> {
        let referenced = self.catalog.referenced_segments()?;
        let candidates: Vec<SegmentId> = self
            .storage
            .segment_ids()
            .await?
            .into_iter()
            .filter(|seg| !referenced.contains(seg) && !self.pins.contains_key(seg))
            .collect();

        let mut reclaimed = 0usize;
        let mut txn = self.storage.begin_txn().await?;
        for seg_id in candidates {
            // One whose metadata can't be read is just as unreferenced.
            let hash = self
                .storage
                .metadata(seg_id)
                .await
                .ok()
                .and_then(|m| m.content_hash);
            txn.delete(seg_id).await?;
            if let Some(hash) = hash {
                let _ = self.catalog.deregister_content(&hash, seg_id)?;
            }
            reclaimed += 1;
        }
        txn.commit().await?;

        Ok(reclaimed)
    }

    /// The storage backend segments are written to.
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// The keyring, when the pipeline has one.
    pub fn keyring(&self) -> Option<&K> {
        self.keyring.as_ref()
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
    fn test_noop_decrypt_refuses_without_a_key() {
        let enc = NoopEncryptor;
        let ctx = DecryptContext::default();

        let err = enc
            .decrypt(
                b"ciphertext",
                &EncryptionPolicy::Disabled,
                SegmentId(1),
                &ctx,
            )
            .expect_err("noop decrypt must not hand back stored bytes");
        assert!(err.to_string().contains("SPACE_MASTER_KEY"), "{err}");
    }

    #[test]
    fn test_noop_mac_methods_refuse_without_a_key() {
        let enc = NoopEncryptor;
        let ctx = DecryptContext::default();
        let err = enc
            .verify_mac(b"any data", SegmentId(1), &ctx)
            .expect_err("noop verify_mac must not pass bytes it can't check");
        assert!(err.to_string().contains("SPACE_MASTER_KEY"), "{err}");
        let err = enc
            .compute_mac(b"any data", SegmentId(1), &ctx)
            .expect_err("noop compute_mac must not return an empty tag");
        assert!(err.to_string().contains("SPACE_MASTER_KEY"), "{err}");
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

    fn test_manifest(
        id: CapsuleId,
        size: u64,
        policy: &Policy,
        segments: &[SegmentId],
        stats: &common::traits::DedupStats,
    ) -> CapsuleManifest {
        CapsuleManifest {
            capsule_id: id,
            created_at: 0,
            size,
            digest: [0u8; 32],
            dedupe: true,
            requires_encryption: false,
            deduped_bytes: stats.bytes_saved,
            policy: policy.clone(),
            entries: segments
                .iter()
                .map(|s| ManifestEntry {
                    segment_id: *s,
                    plain_len: 0,
                    content_hash: [0u8; 32],
                })
                .collect(),
        }
    }

    #[test]
    fn catalog_create_and_lookup_capsule() {
        use common::traits::DedupStats;
        let catalog = InMemoryCatalog::new();
        let id = CapsuleId(uuid::Uuid::new_v4());
        let policy = Policy::default();
        let stats = DedupStats::new();
        catalog
            .create_capsule(test_manifest(id, 100, &policy, &[], &stats))
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
            .create_capsule(test_manifest(id, 200, &policy, &[seg], &stats))
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
            .create_capsule(test_manifest(id, 50, &policy, &[], &stats))
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

    /// Drain a stream, as `spacectl read` and an S3 GET do: the bytes, and
    /// the error that ended it, if one did.
    async fn drain(mut stream: DataStream) -> (Vec<u8>, Option<anyhow::Error>) {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => out.extend_from_slice(&bytes),
                Err(err) => return (out, Some(err)),
            }
        }
        (out, None)
    }

    /// Every full read of a capsule checks its digest, whichever path
    /// serves it. A range can't, and still reads (#190).
    #[tokio::test]
    async fn every_full_read_checks_the_digest() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let data: Vec<u8> = (0..100u8).collect();
        let id = pipeline
            .write_capsule(&data, &Policy::default())
            .await
            .unwrap();
        // The segments no longer make up the bytes the digest names.
        pipeline
            .catalog()
            .inner
            .lock()
            .unwrap()
            .capsules
            .get_mut(&id)
            .unwrap()
            .digest = Some([9; 32]);

        let err = pipeline.read_capsule(id).await.unwrap_err();
        assert!(err.to_string().contains("digest"), "{err}");
        let (bytes, err) = drain(pipeline.read_capsule_stream(id).await.unwrap()).await;
        assert_eq!(bytes, data, "the bytes come before the error");
        assert!(err.unwrap().to_string().contains("digest"));
        assert!(pipeline.read_range(id, 0, 1000).await.is_err());
        let (_, err) = drain(pipeline.read_range_stream(id, 0, 100).await.unwrap()).await;
        assert!(
            err.is_some(),
            "a range covering the whole capsule is a full read"
        );

        assert_eq!(
            pipeline.read_range(id, 10, 20).await.unwrap(),
            &data[10..30]
        );
        let (bytes, err) = drain(pipeline.read_range_stream(id, 1, 99).await.unwrap()).await;
        assert_eq!((bytes.as_slice(), err.is_none()), (&data[1..], true));

        // A capsule recorded without a digest reads as before.
        pipeline
            .catalog()
            .inner
            .lock()
            .unwrap()
            .capsules
            .get_mut(&id)
            .unwrap()
            .digest = None;
        assert_eq!(pipeline.read_capsule(id).await.unwrap(), data);
    }

    /// Segments that end before the capsule's size are an error, not a
    /// short read, with or without a digest.
    #[tokio::test]
    async fn segments_short_of_the_size_are_an_error() {
        let mut pipeline: InMemoryPipeline = PipelineBuilder::new().build();
        let data = b"short".to_vec();
        let id = pipeline
            .write_capsule(&data, &Policy::default())
            .await
            .unwrap();
        {
            let catalog = pipeline.catalog();
            let mut inner = catalog.inner.lock().unwrap();
            let capsule = inner.capsules.get_mut(&id).unwrap();
            capsule.size = 9;
            capsule.digest = None;
        }
        let err = pipeline.read_capsule(id).await.unwrap_err();
        assert!(err.to_string().contains("short of the 9"), "{err}");
        assert!(pipeline.read_range(id, 2, 7).await.is_err());
        assert_eq!(pipeline.read_range(id, 1, 3).await.unwrap(), b"hor");
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

    #[tokio::test]
    async fn read_without_a_key_fails_instead_of_returning_ciphertext() {
        let mut keyed = xts_pipeline();
        // Two segments, so the range read skips one and decodes the other.
        let policy = with_segment_size(encrypted_policy(), 64);
        let data: Vec<u8> = (0..100u8).collect();
        let id = keyed.write_capsule(&data, &policy).await.unwrap();
        assert_eq!(keyed.catalog.lookup_capsule(id).unwrap().segments.len(), 2);

        // The same store, opened without a key (#155).
        let keyless: InMemoryPipeline = PipelineBuilder::new()
            .with_storage(keyed.storage.clone())
            .with_catalog(keyed.catalog.clone())
            .build();

        let err = keyless
            .read_capsule(id)
            .await
            .expect_err("read_capsule returned bytes without a key");
        assert!(format!("{err:#}").contains("SPACE_MASTER_KEY"), "{err:#}");
        assert!(keyless.read_range(id, 70, 10).await.is_err());
        let mut stream = keyless.read_range_stream(id, 0, 100).await.unwrap();
        assert!(
            stream.next().await.unwrap().is_err(),
            "the stream yielded bytes without a key"
        );
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
    async fn garbage_collection_keeps_what_writes_and_dropped_capsules_hold() {
        let mut p: InMemoryPipeline = PipelineBuilder::new().build();
        let policy = small_segments(1000);
        // Clones share the backend's data.
        let mut storage = p.storage().clone();

        // An open write's segments aren't referenced by any capsule yet.
        let mut write = p.begin_write(&policy).unwrap();
        p.write_chunk(&mut write, &noise(1000, 4)).await.unwrap();
        assert_eq!(p.garbage_collect().await.unwrap(), 0);
        let id = p.finish_write(write).await.unwrap();
        assert_eq!(p.read_capsule(id).await.unwrap(), noise(1000, 4));

        // A dropped capsule's segments stay until it's released.
        let dropped = p.commit(vec![JournalRecord::Drop(id)]).unwrap();
        let seg = dropped[0].segments[0];
        assert_eq!(p.garbage_collect().await.unwrap(), 0);
        assert!(storage.metadata(seg).await.is_ok());
        p.release(&dropped[0]).await.unwrap();
        assert!(storage.metadata(seg).await.is_err());

        // An orphan goes. A segment a capsule references stays, whatever its
        // stored ref count says.
        let kept = p.write_capsule(&noise(1000, 5), &policy).await.unwrap();
        let kept_seg = p.catalog().lookup_capsule(kept).unwrap().segments[0];
        let mut meta = storage.metadata(kept_seg).await.unwrap();
        meta.ref_count = 0;
        let orphan = Segment {
            id: SegmentId(999),
            len: 6,
            ref_count: 1,
            ..Segment::default()
        };
        let mut txn = storage.begin_txn().await.unwrap();
        txn.set_segment_metadata(kept_seg, meta).await.unwrap();
        txn.append(SegmentId(999), b"orphan").await.unwrap();
        txn.set_segment_metadata(SegmentId(999), orphan)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(p.garbage_collect().await.unwrap(), 1);
        assert!(storage.metadata(SegmentId(999)).await.is_err());
        assert_eq!(p.read_capsule(kept).await.unwrap(), noise(1000, 5));
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

use std::borrow::Cow;

use anyhow::Result;
use futures::future::BoxFuture;

use crate::{
    Capsule, CapsuleId, CompressionPolicy, ContentHash, EncryptionPolicy, Policy, Segment,
    SegmentId,
};

/// Summary information produced by a compression engine.
#[derive(Debug, Clone)]
pub struct CompressionSummary {
    pub original_size: usize,
    pub output_size: usize,
    pub algorithm: String,
    pub compressed: bool,
    pub reused_input: bool,
    pub reason: Option<String>,
}

impl CompressionSummary {
    pub fn new(original_size: usize, output_size: usize, algorithm: impl Into<String>) -> Self {
        Self {
            original_size,
            output_size,
            algorithm: algorithm.into(),
            compressed: output_size < original_size,
            reused_input: false,
            reason: None,
        }
    }

    pub fn ratio(&self) -> f32 {
        if self.output_size == 0 {
            return 1.0;
        }
        self.original_size as f32 / self.output_size as f32
    }
}

/// Result metadata returned by encryptors.
#[derive(Debug, Clone)]
pub struct EncryptionSummary {
    pub algorithm: String,
    pub key_version: Option<u32>,
    pub encryption_version: Option<u16>,
    pub mac: Option<Vec<u8>>,
    pub tweak_nonce: Option<[u8; 16]>,
    pub integrity_tag: Option<[u8; 16]>,
}

impl EncryptionSummary {
    pub fn new(algorithm: impl Into<String>) -> Self {
        Self {
            algorithm: algorithm.into(),
            key_version: None,
            encryption_version: None,
            mac: None,
            tweak_nonce: None,
            integrity_tag: None,
        }
    }
}

/// Deduplication statistics gathered while processing capsules.
#[derive(Debug, Clone, Default)]
pub struct DedupStats {
    pub total_segments: usize,
    pub deduped_segments: usize,
    pub bytes_saved: u64,
    pub dedup_ratio: f32,
}

impl DedupStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_segment(&mut self, segment_len: u64, was_deduped: bool) {
        self.record(segment_len, was_deduped);
    }

    pub fn compute_ratio(&mut self) {
        if self.bytes_saved > 0 && self.total_segments > 0 {
            let total_bytes = self.total_segments as u64 * crate::SEGMENT_SIZE as u64;
            self.dedup_ratio =
                total_bytes as f32 / (total_bytes.saturating_sub(self.bytes_saved)) as f32;
        }
    }

    pub fn record(&mut self, segment_len: u64, was_deduped: bool) {
        self.total_segments += 1;
        if was_deduped {
            self.deduped_segments += 1;
            self.bytes_saved += segment_len;
        }
    }
}

/// Planned replication behaviour for a capsule write.
#[derive(Debug, Clone, Default)]
pub struct ReplicationStrategy {
    pub synchronous: bool,
    pub targets: Vec<String>,
}

/// Trait implemented by compression engines.
pub trait Compressor: Send + Sync {
    fn compress<'a>(
        &'a self,
        data: &'a [u8],
        policy: &CompressionPolicy,
    ) -> Result<(Cow<'a, [u8]>, CompressionSummary)>;

    fn decompress(&self, data: &[u8], algorithm: &str) -> Result<Vec<u8>>;

    fn supports_algorithm(&self, algorithm: &str) -> bool {
        let _ = algorithm;
        false
    }
}

/// Trait implemented by deduplication engines.
pub trait Deduper: Send + Sync {
    fn hash_content(&self, data: &[u8]) -> ContentHash;

    fn check_dedup(&self, hash: &ContentHash) -> Option<SegmentId>;

    fn register_content(&mut self, hash: ContentHash, segment: SegmentId) -> Result<()>;

    fn update_stats(&mut self, segment_len: u64, was_deduped: bool);

    fn stats(&self) -> DedupStats;
}

/// Trait implemented by encryption engines.
pub trait Encryptor: Send + Sync {
    fn encrypt(
        &self,
        data: Cow<'_, [u8]>,
        policy: &EncryptionPolicy,
        segment: SegmentId,
    ) -> Result<(Vec<u8>, EncryptionSummary)>;

    fn decrypt(
        &self,
        data: &[u8],
        policy: &EncryptionPolicy,
        segment: SegmentId,
    ) -> Result<Vec<u8>>;

    fn compute_mac(&self, data: &[u8], segment: SegmentId) -> Result<Vec<u8>>;

    fn verify_mac(&self, data: &[u8], mac: &[u8], segment: SegmentId) -> Result<()>;
}

/// Transaction object returned by storage backends.
pub trait StorageTransaction: Send {
    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    fn set_segment_metadata<'a>(
        &'a mut self,
        _segment: SegmentId,
        _metadata: Segment,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a mut self, _segment: SegmentId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn commit(self) -> BoxFuture<'static, Result<()>>
    where
        Self: Sized;

    fn rollback(self) -> BoxFuture<'static, Result<()>>
    where
        Self: Sized;
}

/// Storage backend abstraction used by the pipeline.
pub trait StorageBackend: Send + Sync {
    type Transaction: StorageTransaction;

    fn append<'a>(&'a mut self, segment: SegmentId, data: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>>;

    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>>;

    fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, Result<()>>;

    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>>;

    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>>;
}

/// Evaluates policy directives for a given capsule write.
pub trait PolicyEvaluator: Send + Sync {
    fn evaluate_compression(&self, policy: &Policy, sample: &[u8]) -> Result<CompressionPolicy>;

    fn evaluate_dedup(&self, policy: &Policy) -> Result<bool>;

    fn evaluate_encryption(&self, policy: &Policy) -> Result<EncryptionPolicy>;

    fn evaluate_replication(&self, policy: &Policy) -> Result<ReplicationStrategy>;
}

/// Abstract key management used by the encryptor.
pub trait Keyring: Send + Sync {
    fn derive_key(&self, capsule: CapsuleId, segment: SegmentId) -> Result<[u8; 32]>;

    fn rotate_key(&mut self, capsule: CapsuleId) -> Result<()>;
}

/// Protocol view abstraction for front-end handlers.
pub trait ProtocolView {
    type Request;
    type Response;

    fn handle_request(&self, request: Self::Request) -> Result<Self::Response>;
}

/// Metadata catalog abstraction backed by capsule-registry.
pub trait CapsuleCatalog: Send + Sync {
    fn allocate_segment(&self) -> Result<SegmentId>;

    fn lookup_capsule(&self, id: CapsuleId) -> Result<Capsule>;

    fn create_capsule(
        &self,
        id: CapsuleId,
        size: u64,
        policy: &Policy,
        segments: Vec<SegmentId>,
        stats: &DedupStats,
    ) -> Result<()>;

    fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule>;

    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId>;

    fn register_content(&self, hash: ContentHash, segment: SegmentId) -> Result<()>;

    fn deregister_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool>;

    fn capsules(&self) -> Vec<Capsule>;

    fn content_entries(&self) -> Vec<(ContentHash, SegmentId)>;
}

//! Inbound Replication Handler
//!
//! Implements the complete inbound replication flow:
//! 1. Read framed segment data from TCP stream
//! 2. Validate integrity (BLAKE3 MAC)
//! 3. Decrypt segment (XTS-AES-256)
//! 4. Compute content hash for deduplication
//! 5. Check for existing segments (via trait)
//! 6. Persist to NvramLog if new
//! 7. Update metadata and emit telemetry

use anyhow::{anyhow, Result};
use blake3;
use common::{ContentHash, Segment, SegmentId};
use encryption::keymanager::KeyManager;
use encryption::mac::verify_mac;
use encryption::policy::EncryptionMetadata;
use encryption::xts::decrypt_segment;
use nvram_sim::NvramLog;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Trait for content lookup and registration (to avoid circular dependency with capsule-registry)
pub trait ContentStore: Send + Sync {
    /// Look up content by hash, returns segment ID if exists
    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId>;
    /// Register content with hash and segment ID
    fn register_content(&mut self, hash: &ContentHash, segment_id: SegmentId);
}

/// Wire protocol frame for segment replication
///
/// Format:
/// - 4 bytes: frame length (u32 little-endian)
/// - N bytes: bincode-serialized ReplicationFrame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationFrame {
    /// Segment ID being replicated
    pub segment_id: SegmentId,
    /// Encryption metadata (includes key version, tweak, MAC tag)
    pub metadata: EncryptionMetadata,
    /// Encrypted segment data (ciphertext)
    pub encrypted_data: Vec<u8>,
}

impl ReplicationFrame {
    /// Create a new replication frame
    pub fn new(
        segment_id: SegmentId,
        metadata: EncryptionMetadata,
        encrypted_data: Vec<u8>,
    ) -> Self {
        Self {
            segment_id,
            metadata,
            encrypted_data,
        }
    }

    /// Serialize frame to bytes with length prefix
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload = bincode::serialize(self)
            .map_err(|e| anyhow!("failed to serialize frame: {}", e))?;

        let len = payload.len() as u32;
        let mut buf = Vec::with_capacity(4 + payload.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&payload);

        Ok(buf)
    }

    /// Deserialize frame from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data).map_err(|e| anyhow!("failed to deserialize frame: {}", e))
    }
}

/// Replication handler for incoming mirror connections
pub struct ReplicationHandler<C: ContentStore> {
    content_store: Arc<RwLock<C>>,
    nvram_log: Arc<RwLock<NvramLog>>,
    key_manager: Arc<RwLock<KeyManager>>,
}

impl<C: ContentStore> ReplicationHandler<C> {
    /// Create a new replication handler
    pub fn new(
        content_store: Arc<RwLock<C>>,
        nvram_log: Arc<RwLock<NvramLog>>,
        key_manager: Arc<RwLock<KeyManager>>,
    ) -> Self {
        Self {
            content_store,
            nvram_log,
            key_manager,
        }
    }

    /// Handle an incoming mirror connection
    ///
    /// This is the main entry point for inbound replication. It reads frames from the
    /// TCP stream, validates integrity, decrypts, deduplicates, and persists segments.
    pub async fn handle_connection(&self, mut stream: TcpStream) {
        debug!("handling inbound replication connection");

        // Read frame length (4 bytes)
        let frame_len = match self.read_frame_length(&mut stream).await {
            Ok(len) => len,
            Err(e) => {
                error!(error = %e, "failed to read frame length");
                return;
            }
        };

        debug!(frame_len, "read frame length");

        // Read frame data
        let frame_data = match self.read_frame_data(&mut stream, frame_len).await {
            Ok(data) => data,
            Err(e) => {
                error!(error = %e, "failed to read frame data");
                return;
            }
        };

        // Deserialize frame
        let frame = match ReplicationFrame::from_bytes(&frame_data) {
            Ok(f) => f,
            Err(e) => {
                error!(error = %e, "failed to deserialize frame");
                return;
            }
        };

        debug!(
            segment_id = frame.segment_id.0,
            data_len = frame.encrypted_data.len(),
            "deserialized replication frame"
        );

        // Process the replicated segment
        if let Err(e) = self.process_segment(frame).await {
            error!(error = %e, "failed to process replicated segment");
        }
    }

    /// Read frame length from stream
    async fn read_frame_length(&self, stream: &mut TcpStream) -> Result<u32> {
        let mut len_bytes = [0u8; 4];
        stream
            .read_exact(&mut len_bytes)
            .await
            .map_err(|e| anyhow!("failed to read frame length: {}", e))?;

        Ok(u32::from_le_bytes(len_bytes))
    }

    /// Read frame data from stream
    async fn read_frame_data(&self, stream: &mut TcpStream, len: u32) -> Result<Vec<u8>> {
        // Sanity check: reject frames larger than 16MB (4MB segment + overhead)
        if len > 16 * 1024 * 1024 {
            return Err(anyhow!("frame length {} exceeds maximum (16MB)", len));
        }

        let mut buf = vec![0u8; len as usize];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| anyhow!("failed to read frame data: {}", e))?;

        Ok(buf)
    }

    /// Process a replicated segment
    ///
    /// Flow:
    /// 1. Validate MAC integrity
    /// 2. Decrypt segment
    /// 3. Compute content hash
    /// 4. Check for deduplication
    /// 5. Persist if new
    /// 6. Update metadata
    async fn process_segment(&self, frame: ReplicationFrame) -> Result<()> {
        let segment_id = frame.segment_id;
        let metadata = frame.metadata;
        let ciphertext = frame.encrypted_data;

        // Step 1: Validate MAC
        debug!(segment_id = segment_id.0, "validating MAC");
        let key_version = metadata
            .key_version
            .ok_or_else(|| anyhow!("missing key version in metadata"))?;

        let key_manager = self.key_manager.read().await;
        let key_pair = key_manager
            .get_key(key_version)
            .map_err(|e| anyhow!("failed to get key {}: {}", key_version, e))?;

        if let Err(e) = verify_mac(
            &ciphertext,
            &metadata,
            key_pair.key1(),
            key_pair.key2(),
        ) {
            warn!(
                segment_id = segment_id.0,
                error = %e,
                "MAC validation failed for replicated segment"
            );
            return Err(anyhow!("MAC validation failed: {}", e));
        }

        debug!(segment_id = segment_id.0, "MAC validation successful");

        // Step 2: Decrypt segment
        debug!(segment_id = segment_id.0, "decrypting segment");
        let plaintext = decrypt_segment(&ciphertext, key_pair, &metadata)
            .map_err(|e| anyhow!("decryption failed: {}", e))?;

        debug!(
            segment_id = segment_id.0,
            plaintext_len = plaintext.len(),
            "decryption successful"
        );

        // Release key_manager lock before long operations
        drop(key_manager);

        // Step 3: Compute content hash for deduplication
        let content_hash = ContentHash::from_bytes(blake3::hash(&plaintext).as_bytes());
        debug!(
            segment_id = segment_id.0,
            content_hash = %content_hash.as_str(),
            "computed content hash"
        );

        // Step 4: Check for deduplication
        let content_store = self.content_store.read().await;
        if let Some(existing_id) = content_store.lookup_content(&content_hash) {
            info!(
                segment_id = segment_id.0,
                existing_id = existing_id.0,
                content_hash = %content_hash.as_str(),
                "segment already exists (dedup hit)"
            );

            // Update reference count
            drop(content_store); // Release read lock
            let mut nvram_log = self.nvram_log.write().await;
            nvram_log.increment_refcount(existing_id)?;
            debug!(
                existing_id = existing_id.0,
                "incremented refcount for deduplicated segment"
            );

            return Ok(());
        }
        drop(content_store); // Release read lock before persistence

        // Step 5: Persist to NvramLog (segment is new)
        debug!(
            segment_id = segment_id.0,
            ciphertext_len = ciphertext.len(),
            "persisting new segment to NvramLog"
        );

        let mut nvram_log = self.nvram_log.write().await;

        // Create segment metadata
        let segment = Segment {
            id: segment_id,
            offset: 0, // Will be set by append()
            len: ciphertext.len() as u32,
            compressed: false, // Assume compression already applied
            compression_algo: String::new(),
            content_hash: Some(content_hash.clone()),
            ref_count: 1,
            deduplicated: false,
            access_count: 0,
            encryption_version: metadata.encryption_version,
            key_version: metadata.key_version,
            tweak_nonce: metadata.tweak_nonce,
            integrity_tag: metadata.integrity_tag,
            encrypted: true,
            pq_ciphertext: None,
            pq_nonce: None,
        };

        // Append to NVRAM log
        let segment_metadata = nvram_log
            .append(segment_id, &ciphertext)
            .map_err(|e| anyhow!("failed to append segment to NvramLog: {}", e))?;

        info!(
            segment_id = segment_id.0,
            offset = segment_metadata.offset,
            len = segment_metadata.len,
            "persisted segment to NvramLog"
        );

        drop(nvram_log); // Release write lock

        // Step 6: Register in ContentStore for dedup lookups
        let mut content_store = self.content_store.write().await;
        content_store.register_content(&content_hash, segment_id);
        debug!(
            segment_id = segment_id.0,
            content_hash = %content_hash.as_str(),
            "registered content hash in store"
        );

        // TODO: Emit telemetry event for scaling agents
        // This would integrate with the ScalingAgent via telemetry channel
        info!(
            segment_id = segment_id.0,
            content_hash = %content_hash.as_str(),
            "inbound replication completed successfully"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replication_frame_serialization() {
        let frame = ReplicationFrame {
            segment_id: SegmentId(42),
            metadata: EncryptionMetadata::new_xts(1, [5u8; 16], 1024),
            encrypted_data: vec![1, 2, 3, 4, 5],
        };

        let bytes = frame.to_bytes().unwrap();

        // Check length prefix
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(len as usize, bytes.len() - 4);

        // Deserialize
        let deserialized = ReplicationFrame::from_bytes(&bytes[4..]).unwrap();
        assert_eq!(deserialized.segment_id.0, 42);
        assert_eq!(deserialized.encrypted_data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_replication_frame_roundtrip() {
        let original = ReplicationFrame {
            segment_id: SegmentId(12345),
            metadata: EncryptionMetadata::new_xts(2, [9u8; 16], 4096),
            encrypted_data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };

        let bytes = original.to_bytes().unwrap();
        let decoded = ReplicationFrame::from_bytes(&bytes[4..]).unwrap();

        assert_eq!(decoded.segment_id.0, 12345);
        assert_eq!(decoded.metadata.key_version, Some(2));
        assert_eq!(decoded.encrypted_data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }
}

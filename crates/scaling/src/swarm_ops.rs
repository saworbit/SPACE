use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use common::podms::TransformOps;
use common::traits::Compressor;
use common::{CapsuleId, CompressionPolicy, EncryptionPolicy, SegmentId};
use compression::Lz4ZstdCompressor;
use encryption::keymanager::XTS_KEY_SIZE;
use encryption::mac::compute_mac;
use encryption::policy::EncryptionMetadata;
use encryption::{xts, KeyManager, XtsKeyPair};
use tokio::sync::RwLock;

/// SwarmOps adapts the low-level crypto/compression crates to the PODMS
/// `TransformOps` interface so capsules can self-transform during migrations.
/// Keys are derived per capsule to satisfy the Zero Trust requirements.
pub struct SwarmOps {
    key_manager: Arc<RwLock<KeyManager>>,
    compressor: Lz4ZstdCompressor,
}

impl SwarmOps {
    pub fn new(key_manager: Arc<RwLock<KeyManager>>) -> Self {
        Self {
            key_manager,
            compressor: Lz4ZstdCompressor::new(),
        }
    }

    /// Derive capsule-specific XTS key material from the shared KeyManager.
    pub fn derive_capsule_key(
        key_manager: &RwLock<KeyManager>,
        capsule_id: CapsuleId,
        key_version: Option<u32>,
    ) -> Result<(XtsKeyPair, u32)> {
        let mut manager = key_manager.blocking_write();
        let version = key_version.unwrap_or_else(|| manager.current_version());
        let base = manager
            .get_key(version)
            .map_err(|e| anyhow!("failed to load key version {version}: {e}"))?
            .clone();
        drop(manager);

        // Mix capsule_id into the base key material to produce a per-capsule XTS pair.
        let mut hasher = Hasher::new();
        hasher.update(base.key1());
        hasher.update(base.key2());
        hasher.update(capsule_id.as_uuid().as_bytes());
        hasher.update(&version.to_le_bytes());

        let mut derived = [0u8; XTS_KEY_SIZE];
        hasher.finalize_xof().fill(&mut derived);
        Ok((XtsKeyPair::from_bytes(derived), version))
    }

    /// Derive a deterministic segment key from content hash (convergent path for dedup).
    pub fn derive_segment_key(content_hash: &[u8]) -> XtsKeyPair {
        let mut hasher = Hasher::new();
        hasher.update(b"SPACE::SEGMENT_KEY_V1");
        hasher.update(content_hash);
        let mut derived = [0u8; XTS_KEY_SIZE];
        hasher.finalize_xof().fill(&mut derived);
        XtsKeyPair::from_bytes(derived)
    }

    /// Wrap a segment key using the capsule-specific key (envelope encryption).
    pub fn wrap_segment_key(capsule_key: &XtsKeyPair, segment_key: &XtsKeyPair) -> Vec<u8> {
        let mut keystream = [0u8; XTS_KEY_SIZE];
        let mut hasher = Hasher::new_keyed(capsule_key.key1());
        hasher.update(capsule_key.key2());
        hasher.update(b"SPACE::SEGMENT_KEY_WRAP_V1");
        hasher.finalize_xof().fill(&mut keystream);

        let segment_bytes = segment_key.to_bytes();
        let mut wrapped = [0u8; XTS_KEY_SIZE];
        for (i, byte) in segment_bytes.iter().enumerate() {
            wrapped[i] = *byte ^ keystream[i];
        }
        wrapped.to_vec()
    }

    /// Unwrap a segment key using the capsule-specific key.
    pub fn unwrap_segment_key(capsule_key: &XtsKeyPair, wrapped: &[u8]) -> Result<XtsKeyPair> {
        if wrapped.len() != XTS_KEY_SIZE {
            return Err(anyhow!(
                "wrapped segment key length {} invalid (expected {})",
                wrapped.len(),
                XTS_KEY_SIZE
            ));
        }
        let mut keystream = [0u8; XTS_KEY_SIZE];
        let mut hasher = Hasher::new_keyed(capsule_key.key1());
        hasher.update(capsule_key.key2());
        hasher.update(b"SPACE::SEGMENT_KEY_WRAP_V1");
        hasher.finalize_xof().fill(&mut keystream);

        let mut unwrapped = [0u8; XTS_KEY_SIZE];
        for (i, byte) in wrapped.iter().enumerate() {
            unwrapped[i] = *byte ^ keystream[i];
        }
        Ok(XtsKeyPair::from_bytes(unwrapped))
    }

    pub fn derive_xts_tweak(segment_id: SegmentId) -> [u8; 16] {
        let mut tweak = [0u8; 16];
        tweak[..8].copy_from_slice(&segment_id.0.to_le_bytes());
        tweak
    }

    fn decompress_with_policy(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        match policy {
            CompressionPolicy::None => Ok(data.to_vec()),
            CompressionPolicy::LZ4 { level } => self
                .compressor
                .decompress(data, &format!("lz4:{level}"))
                .context("lz4 decompression failed"),
            CompressionPolicy::Zstd { level } => self
                .compressor
                .decompress(data, &format!("zstd:{level}"))
                .context("zstd decompression failed"),
        }
    }

    fn compress_with_policy(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        match policy {
            CompressionPolicy::None => Ok(data.to_vec()),
            CompressionPolicy::LZ4 { .. } | CompressionPolicy::Zstd { .. } => {
                let (view, _summary) = self.compressor.compress(data, policy)?;
                Ok(view.into_owned())
            }
        }
    }
}

impl TransformOps for SwarmOps {
    fn decrypt(
        &self,
        capsule_id: CapsuleId,
        data: &[u8],
        policy: &EncryptionPolicy,
        ctx: SegmentId,
    ) -> Result<Vec<u8>> {
        match policy {
            EncryptionPolicy::Disabled => Ok(data.to_vec()),
            EncryptionPolicy::XtsAes256 { key_version } => {
                let (key, _) =
                    Self::derive_capsule_key(&self.key_manager, capsule_id, *key_version)?;
                let tweak = Self::derive_xts_tweak(ctx);
                xts::decrypt(data, &key, &tweak).context("xts decrypt failed")
            }
        }
    }

    fn encrypt(
        &self,
        capsule_id: CapsuleId,
        data: &[u8],
        policy: &EncryptionPolicy,
        ctx: SegmentId,
    ) -> Result<Vec<u8>> {
        match policy {
            EncryptionPolicy::Disabled => Ok(data.to_vec()),
            EncryptionPolicy::XtsAes256 { key_version } => {
                let (key, _) =
                    Self::derive_capsule_key(&self.key_manager, capsule_id, *key_version)?;
                let tweak = Self::derive_xts_tweak(ctx);
                xts::encrypt(data, &key, &tweak).context("xts encrypt failed")
            }
        }
    }

    fn decompress(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        self.decompress_with_policy(data, policy)
    }

    fn compress(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        self.compress_with_policy(data, policy)
    }
}

impl SwarmOps {
    /// Encrypt data and produce encryption metadata (including MAC) for replication frames.
    pub fn encrypt_with_metadata(
        &self,
        capsule_id: CapsuleId,
        data: &[u8],
        policy: &EncryptionPolicy,
        ctx: SegmentId,
    ) -> Result<(Vec<u8>, EncryptionMetadata)> {
        match policy {
            EncryptionPolicy::Disabled => {
                Ok((data.to_vec(), EncryptionMetadata::new_unencrypted()))
            }
            EncryptionPolicy::XtsAes256 { key_version } => {
                let content_hash = blake3::hash(data);
                let segment_key = Self::derive_segment_key(content_hash.as_bytes());
                let tweak = Self::derive_xts_tweak(ctx);
                let ciphertext =
                    xts::encrypt(data, &segment_key, &tweak).context("xts encrypt failed")?;

                let (capsule_key, version) =
                    Self::derive_capsule_key(&self.key_manager, capsule_id, *key_version)?;
                let wrapped_key = Self::wrap_segment_key(&capsule_key, &segment_key);

                let mut metadata =
                    EncryptionMetadata::new_xts(version, tweak, ciphertext.len() as u32);
                metadata.wrapped_segment_key = Some(wrapped_key);
                let mac = compute_mac(
                    &ciphertext,
                    &metadata,
                    capsule_key.key1(),
                    capsule_key.key2(),
                )?;
                metadata.set_integrity_tag(mac);
                Ok((ciphertext, metadata))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_manager() -> Arc<RwLock<KeyManager>> {
        Arc::new(RwLock::new(KeyManager::new([42u8; 32])))
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let ops = SwarmOps::new(key_manager());
        let capsule = CapsuleId::new();
        let policy = EncryptionPolicy::XtsAes256 { key_version: None };
        let plaintext = b"SwarmOps XTS round trip payload".repeat(2);
        let segment = SegmentId(7);

        let encrypted = ops.encrypt(capsule, &plaintext, &policy, segment).unwrap();
        assert_ne!(encrypted, plaintext);

        let decrypted = ops.decrypt(capsule, &encrypted, &policy, segment).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn per_capsule_keys_produce_unique_ciphertext() {
        let ops = SwarmOps::new(key_manager());
        let capsule_a = CapsuleId::new();
        let capsule_b = CapsuleId::new();
        let policy = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        let plaintext = vec![9u8; 64];
        let segment = SegmentId(99);

        let ct_a = ops
            .encrypt(capsule_a, &plaintext, &policy, segment)
            .unwrap();
        let ct_b = ops
            .encrypt(capsule_b, &plaintext, &policy, segment)
            .unwrap();
        assert_ne!(ct_a, ct_b);

        assert_eq!(
            ops.decrypt(capsule_a, &ct_a, &policy, segment).unwrap(),
            plaintext
        );
        assert_eq!(
            ops.decrypt(capsule_b, &ct_b, &policy, segment).unwrap(),
            plaintext
        );
    }

    #[test]
    fn compression_round_trip() {
        let ops = SwarmOps::new(key_manager());
        let payload = b"compress me!".repeat(128);
        let policy = CompressionPolicy::Zstd { level: 6 };

        let compressed = ops.compress(&payload, &policy).unwrap();
        assert!(compressed.len() < payload.len());

        let decompressed = ops.decompress(&compressed, &policy).unwrap();
        assert_eq!(decompressed, payload);
    }
}

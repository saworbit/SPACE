use anyhow::{anyhow, Context, Result};
use common::podms::TransformOps;
use common::{CapsuleId, CompressionPolicy, EncryptionPolicy, SegmentId};
use encryption::keymanager::MASTER_KEY_SIZE;
use encryption::{xts, EncryptionMetadata, KeyManager};
use nvram_sim::NvramLog;
use std::sync::{Arc, Mutex};

/// TransformOps implementation that performs real encryption/decryption for registry data.
pub struct RegistryTransformOps {
    key_manager: Arc<Mutex<KeyManager>>,
    nvram: Option<Arc<NvramLog>>,
}

impl RegistryTransformOps {
    pub fn new(key_manager: Arc<Mutex<KeyManager>>) -> Self {
        Self {
            key_manager,
            nvram: None,
        }
    }

    pub fn with_nvram(key_manager: Arc<Mutex<KeyManager>>, nvram: Arc<NvramLog>) -> Self {
        Self {
            key_manager,
            nvram: Some(nvram),
        }
    }

    fn derive_tweak(&self, capsule_id: CapsuleId, segment_id: SegmentId) -> [u8; 16] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(capsule_id.as_uuid().as_bytes());
        hasher.update(&segment_id.0.to_le_bytes());
        let hash = hasher.finalize();
        let mut tweak = [0u8; 16];
        tweak.copy_from_slice(&hash.as_bytes()[..16]);
        tweak
    }

    fn resolve_key_and_tweak(
        &self,
        capsule_id: CapsuleId,
        segment_id: SegmentId,
        policy: &EncryptionPolicy,
    ) -> Result<(u32, [u8; 16])> {
        let segment_meta = self
            .nvram
            .as_ref()
            .and_then(|log| log.get_segment_metadata(segment_id).ok());

        let key_version = segment_meta
            .as_ref()
            .and_then(|meta| meta.key_version)
            .or(match policy {
                EncryptionPolicy::XtsAes256 { key_version } => *key_version,
                EncryptionPolicy::Disabled => None,
            })
            .unwrap_or_else(|| {
                self.key_manager
                    .lock()
                    .map(|km| km.current_version())
                    .unwrap_or(1)
            });

        let tweak = segment_meta
            .and_then(|meta| meta.tweak_nonce)
            .unwrap_or_else(|| self.derive_tweak(capsule_id, segment_id));

        Ok((key_version, tweak))
    }
}

impl TransformOps for RegistryTransformOps {
    fn decrypt(
        &self,
        capsule_id: CapsuleId,
        data: &[u8],
        policy: &EncryptionPolicy,
        segment_id: SegmentId,
    ) -> Result<Vec<u8>> {
        if !policy.is_enabled() || data.is_empty() {
            return Ok(data.to_vec());
        }

        let (key_version, tweak) = self.resolve_key_and_tweak(capsule_id, segment_id, policy)?;

        let mut manager = self
            .key_manager
            .lock()
            .map_err(|_| anyhow!("key manager mutex poisoned"))?;
        let key_pair = manager
            .get_key(key_version)
            .context("failed to fetch XTS key")?
            .clone();
        drop(manager);

        if let Some(meta) = self
            .nvram
            .as_ref()
            .and_then(|log| log.get_segment_metadata(segment_id).ok())
        {
            if let Some(tag) = meta.integrity_tag {
                let enc_meta = EncryptionMetadata {
                    encryption_version: meta.encryption_version,
                    key_version: meta.key_version,
                    wrapped_segment_key: None,
                    tweak_nonce: Some(tweak),
                    integrity_tag: Some(tag),
                    ciphertext_len: Some(data.len() as u32),
                };

                encryption::verify_mac(data, &enc_meta, key_pair.key1(), key_pair.key2())
                    .context("MAC verification failed")?;
            }
        }

        xts::decrypt(data, &key_pair, &tweak).context("XTS decryption failed")
    }

    fn encrypt(
        &self,
        capsule_id: CapsuleId,
        data: &[u8],
        policy: &EncryptionPolicy,
        segment_id: SegmentId,
    ) -> Result<Vec<u8>> {
        if !policy.is_enabled() {
            return Ok(data.to_vec());
        }

        let (requested_version, tweak) =
            self.resolve_key_and_tweak(capsule_id, segment_id, policy)?;

        let mut manager = self
            .key_manager
            .lock()
            .map_err(|_| anyhow!("key manager mutex poisoned"))?;
        let mut key_version = match policy {
            EncryptionPolicy::XtsAes256 { key_version } => key_version.unwrap_or(requested_version),
            EncryptionPolicy::Disabled => requested_version,
        };

        if key_version == 0 {
            key_version = manager.current_version();
        }

        let key_pair = manager
            .get_key(key_version)
            .context("failed to fetch XTS key for encryption")?
            .clone();
        drop(manager);

        let ciphertext = xts::encrypt(data, &key_pair, &tweak)
            .with_context(|| format!("XTS encryption failed (key v{key_version})"))?;

        Ok(ciphertext)
    }

    fn decompress(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        match policy {
            CompressionPolicy::None => Ok(data.to_vec()),
            CompressionPolicy::LZ4 { .. } => compression::decompress_lz4(data).map_err(Into::into),
            CompressionPolicy::Zstd { .. } => {
                compression::decompress_zstd(data).map_err(Into::into)
            }
        }
    }

    fn compress(&self, data: &[u8], policy: &CompressionPolicy) -> Result<Vec<u8>> {
        let (compressed, _) = compression::compress_segment(data, policy)?;
        Ok(compressed.into_owned())
    }
}

impl Default for RegistryTransformOps {
    fn default() -> Self {
        let master = [0u8; MASTER_KEY_SIZE];
        Self::new(Arc::new(Mutex::new(KeyManager::new(master))))
    }
}

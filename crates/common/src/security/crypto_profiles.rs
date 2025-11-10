use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use pqcrypto_kyber::kyber768::{self, Ciphertext, PublicKey, SecretKey};
use pqcrypto_traits::kem::{Ciphertext as _, PublicKey as _, SecretKey as _, SharedSecret as _};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::{CapsuleId, ContentHash, CryptoProfile, SegmentId};

/// Persistent Kyber key manager that stores the node's keypair on disk.
#[derive(Clone)]
pub struct KyberKeyManager {
    state: Arc<Mutex<KyberKeyMaterialState>>,
}

pub struct KyberKeyMaterialState {
    pub public: PublicKey,
    pub secret: SecretKey,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HybridKeyMaterial {
    pub wrapped_key: [u8; 64],
    pub nonce: [u8; 16],
    pub ciphertext: Vec<u8>,
}

pub trait KyberNonceExt {
    fn mix_with(&self, base: [u8; 16]) -> [u8; 16];
}

impl KyberNonceExt for [u8; 16] {
    fn mix_with(&self, base: [u8; 16]) -> [u8; 16] {
        let mut hasher = Hasher::new();
        hasher.update(&base);
        hasher.update(self);
        let mut reader = hasher.finalize_xof();
        let mut mixed = [0u8; 16];
        reader.fill(&mut mixed);
        mixed
    }
}

impl KyberKeyManager {
    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let material = if path.exists() {
            load_keys(&path)?
        } else {
            let (public, secret) = kyber768::keypair();
            store_keys(&path, &public, &secret)?;
            info!("generated new Kyber keypair at {}", path.display());
            KyberKeyMaterialState {
                public,
                secret,
                path: path.clone(),
            }
        };

        Ok(Self {
            state: Arc::new(Mutex::new(material)),
        })
    }

    pub fn from_env() -> Result<Self> {
        let path = std::env::var("SPACE_KYBER_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("space.kyber.key"));
        Self::load_or_generate(path)
    }

    pub fn wrap_xts_key(
        &self,
        profile: CryptoProfile,
        base_key: &[u8; 64],
        capsule: &CapsuleId,
        segment: SegmentId,
        hash: &ContentHash,
    ) -> Result<Option<HybridKeyMaterial>> {
        if profile != CryptoProfile::HybridKyber {
            return Ok(None);
        }
        let state = self.state.lock().unwrap();
        let (shared, ciphertext) = kyber768::encapsulate(&state.public);
        Ok(Some(derive_material(
            base_key,
            capsule,
            segment,
            hash,
            shared.as_bytes(),
            ciphertext.as_bytes(),
        )))
    }

    pub fn unwrap_xts_key(
        &self,
        profile: CryptoProfile,
        base_key: &[u8; 64],
        capsule: &CapsuleId,
        segment: SegmentId,
        hash: &ContentHash,
        ciphertext_hex: &str,
    ) -> Result<Option<HybridKeyMaterial>> {
        if profile != CryptoProfile::HybridKyber {
            return Ok(None);
        }

        let bytes = hex::decode(ciphertext_hex)?;
        let cipher = Ciphertext::from_bytes(&bytes)
            .map_err(|err| anyhow!("invalid kyber ciphertext: {err:?}"))?;

        let state = self.state.lock().unwrap();
        let shared = kyber768::decapsulate(&cipher, &state.secret);
        Ok(Some(derive_material(
            base_key,
            capsule,
            segment,
            hash,
            shared.as_bytes(),
            cipher.as_bytes(),
        )))
    }
}

fn derive_material(
    base_key: &[u8; 64],
    capsule: &CapsuleId,
    segment: SegmentId,
    hash: &ContentHash,
    shared: &[u8],
    ciphertext: &[u8],
) -> HybridKeyMaterial {
    let mut hasher = Hasher::new();
    hasher.update(base_key);
    hasher.update(shared);
    hasher.update(capsule.as_uuid().as_bytes());
    hasher.update(&segment.0.to_le_bytes());
    hasher.update(hash.as_str().as_bytes());
    hasher.update(ciphertext);

    let mut reader = hasher.finalize_xof();
    let mut wrapped = [0u8; 64];
    reader.fill(&mut wrapped);
    let mut nonce = [0u8; 16];
    reader.fill(&mut nonce);

    HybridKeyMaterial {
        wrapped_key: wrapped,
        nonce,
        ciphertext: ciphertext.to_vec(),
    }
}

fn load_keys(path: &Path) -> Result<KyberKeyMaterialState> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read {}", path.display()))?;
    let disk: StoredKyberKey = serde_json::from_str(&contents)?;
    let public = PublicKey::from_bytes(&hex::decode(disk.public)?)
        .map_err(|err| anyhow!("invalid public key: {err:?}"))?;
    let secret = SecretKey::from_bytes(&hex::decode(disk.secret)?)
        .map_err(|err| anyhow!("invalid secret key: {err:?}"))?;
    Ok(KyberKeyMaterialState {
        public,
        secret,
        path: path.to_path_buf(),
    })
}

fn store_keys(path: &Path, public: &PublicKey, secret: &SecretKey) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let disk = StoredKyberKey {
        public: hex::encode(public.as_bytes()),
        secret: hex::encode(secret.as_bytes()),
    };
    let mut file = File::create(path)?;
    file.write_all(serde_json::to_vec(&disk)?.as_slice())?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct StoredKyberKey {
    public: String,
    secret: String,
}

pub fn collect_base_material(pair: (&[u8; 32], &[u8; 32])) -> [u8; 64] {
    let mut buffer = [0u8; 64];
    buffer[..32].copy_from_slice(pair.0);
    buffer[32..].copy_from_slice(pair.1);
    buffer
}

pub fn serialize_ciphertext(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_and_restore_material() {
        let path = std::env::temp_dir().join("space-kyber-test.key");
        let manager = KyberKeyManager::load_or_generate(&path).unwrap();
        let base_key = [0x42u8; 64];
        let capsule = CapsuleId::new();
        let hash = ContentHash("abc123".into());
        let segment = SegmentId(7);

        let wrapped = manager
            .wrap_xts_key(
                CryptoProfile::HybridKyber,
                &base_key,
                &capsule,
                segment,
                &hash,
            )
            .unwrap()
            .expect("hybrid material");
        let decoded = manager
            .unwrap_xts_key(
                CryptoProfile::HybridKyber,
                &base_key,
                &capsule,
                segment,
                &hash,
                &hex::encode(&wrapped.ciphertext),
            )
            .unwrap()
            .expect("unwrap");

        assert_eq!(wrapped.wrapped_key, decoded.wrapped_key);
        assert_eq!(wrapped.nonce, decoded.nonce);
        std::fs::remove_file(path).ok();
    }
}

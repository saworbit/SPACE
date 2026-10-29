use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::error::{EncryptionError, Result};
use crate::keymanager::MASTER_KEY_SIZE;

#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Fetch the master key material (32 bytes).
    ///
    /// Implementations should avoid leaking the key into logs and should prefer
    /// secure memory handling. The returned buffer is zeroized on drop.
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>>;
}

#[derive(Debug, Clone)]
pub struct EnvKeyProvider {
    var: String,
}

impl EnvKeyProvider {
    pub fn new(var: impl Into<String>) -> Self {
        Self { var: var.into() }
    }
}

impl Default for EnvKeyProvider {
    fn default() -> Self {
        Self::new("SPACE_MASTER_KEY")
    }
}

/// Backwards-compatible name (Phase 6 and earlier).
pub type EnvProvider = EnvKeyProvider;

#[async_trait]
impl KeyProvider for EnvKeyProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        // No fallback key: a missing variable must not silently encrypt
        // under a fixed, public key (#84).
        let key_material = std::env::var(&self.var).map_err(|_| {
            EncryptionError::InvalidConfiguration(format!(
                "{} environment variable not set",
                self.var
            ))
        })?;
        let bytes = parse_key_material(&key_material, &format!("env:{}", self.var))?;
        Ok(Zeroizing::new(bytes))
    }
}

#[derive(Debug, Clone)]
pub struct FileKeyProvider {
    pub path: std::path::PathBuf,
}

impl FileKeyProvider {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// Backwards-compatible name used in some docs/specs.
pub type FileProvider = FileKeyProvider;

#[async_trait]
impl KeyProvider for FileKeyProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        let raw = std::fs::read(&self.path).map_err(|e| {
            EncryptionError::InvalidConfiguration(format!(
                "failed to read master key file {}: {}",
                self.path.display(),
                e
            ))
        })?;

        let trimmed = trim_ascii_whitespace(&raw);

        let bytes = match std::str::from_utf8(trimmed) {
            Ok(s) => parse_key_material(s, &format!("file:{}", self.path.display()))?,
            Err(_) => {
                if trimmed.len() != MASTER_KEY_SIZE {
                    return Err(EncryptionError::InvalidKeyLength {
                        expected: MASTER_KEY_SIZE,
                        actual: trimmed.len(),
                    });
                }
                trimmed.to_vec()
            }
        };

        Ok(Zeroizing::new(bytes))
    }
}

#[derive(Debug, Clone)]
pub struct AwsKmsProvider {
    pub key_id: String,
}

#[async_trait]
impl KeyProvider for AwsKmsProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        Err(EncryptionError::InvalidConfiguration(format!(
            "AwsKmsProvider not implemented (key_id={}); integrate AWS SDK + KMS decrypt",
            self.key_id
        )))
    }
}

#[derive(Debug, Clone)]
pub struct HashiCorpVaultProvider {
    pub address: String,
    pub secret_path: String,
}

#[async_trait]
impl KeyProvider for HashiCorpVaultProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        Err(EncryptionError::InvalidConfiguration(format!(
            "HashiCorpVaultProvider not implemented (addr={}, path={}); integrate Vault transit/kv",
            self.address, self.secret_path
        )))
    }
}

fn parse_key_material(raw: &str, source: &str) -> Result<Vec<u8>> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);

    // Compatibility: many docs use `openssl rand -hex 32` (64 hex chars).
    if raw.len() == MASTER_KEY_SIZE * 2 && raw.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(raw).map_err(|e| {
            EncryptionError::InvalidConfiguration(format!(
                "invalid hex master key in {}: {}",
                source, e
            ))
        })?;
        debug_assert_eq!(bytes.len(), MASTER_KEY_SIZE);
        return Ok(bytes);
    }

    let bytes = raw.as_bytes().to_vec();
    if bytes.len() != MASTER_KEY_SIZE {
        return Err(EncryptionError::InvalidKeyLength {
            expected: MASTER_KEY_SIZE,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = bytes.split_last() {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("env provider should not suspend"),
        }
    }

    #[test]
    fn env_provider_errors_when_variable_is_unset() {
        let provider = EnvKeyProvider::new("SPACE_TEST_KEY_PROVIDER_UNSET");
        std::env::remove_var("SPACE_TEST_KEY_PROVIDER_UNSET");
        assert!(block_on(provider.fetch_master_key()).is_err());
    }

    #[test]
    fn env_provider_reads_hex_key() {
        let provider = EnvKeyProvider::new("SPACE_TEST_KEY_PROVIDER_SET");
        std::env::set_var("SPACE_TEST_KEY_PROVIDER_SET", "07".repeat(32));
        let key = block_on(provider.fetch_master_key()).unwrap();
        std::env::remove_var("SPACE_TEST_KEY_PROVIDER_SET");
        assert_eq!(key.as_slice(), &[7u8; MASTER_KEY_SIZE]);
    }
}

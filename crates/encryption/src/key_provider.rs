use anyhow::Context;
use async_trait::async_trait;

use crate::error::{EncryptionError, Result};
use crate::keymanager::MASTER_KEY_SIZE;

#[async_trait]
pub trait KeyProvider: Send + Sync {
    async fn fetch_master_key(&self) -> Result<[u8; MASTER_KEY_SIZE]>;
}

#[derive(Debug, Clone)]
pub struct EnvProvider {
    var: String,
}

impl EnvProvider {
    pub fn new(var: impl Into<String>) -> Self {
        Self { var: var.into() }
    }
}

impl Default for EnvProvider {
    fn default() -> Self {
        Self::new("SPACE_MASTER_KEY")
    }
}

#[async_trait]
impl KeyProvider for EnvProvider {
    async fn fetch_master_key(&self) -> Result<[u8; MASTER_KEY_SIZE]> {
        let hex_key = std::env::var(&self.var).map_err(|_| {
            EncryptionError::InvalidConfiguration(format!(
                "{} environment variable not set",
                self.var
            ))
        })?;

        let bytes =
            hex::decode(&hex_key).with_context(|| format!("invalid hex in {}", self.var))?;

        if bytes.len() != MASTER_KEY_SIZE {
            return Err(EncryptionError::InvalidKeyLength {
                expected: MASTER_KEY_SIZE,
                actual: bytes.len(),
            });
        }

        let mut master_key = [0u8; MASTER_KEY_SIZE];
        master_key.copy_from_slice(&bytes);
        Ok(master_key)
    }
}

#[derive(Debug, Clone)]
pub struct AwsKmsProvider {
    pub key_id: String,
}

#[async_trait]
impl KeyProvider for AwsKmsProvider {
    async fn fetch_master_key(&self) -> Result<[u8; MASTER_KEY_SIZE]> {
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
    async fn fetch_master_key(&self) -> Result<[u8; MASTER_KEY_SIZE]> {
        Err(EncryptionError::InvalidConfiguration(format!(
            "HashiCorpVaultProvider not implemented (addr={}, path={}); integrate Vault transit/kv",
            self.address, self.secret_path
        )))
    }
}

use anyhow::Result;
use common::traits::DataStream;
use common::{CapsuleId, Policy};
use encryption::KeyManager;
use nvram_sim::NvramLog;
use tokio::io::AsyncRead;

use crate::CapsuleRegistry;

mod modular;
pub mod strategy;

use modular::ModularPipeline;
use strategy::PipelineStrategy;

/// Single-node write/read path. Compression, BLAKE3 dedup, and XTS live in
/// the `pipeline` crate. This type is the registry-facing wrapper.
pub struct WritePipeline {
    inner: ModularPipeline,
}

impl WritePipeline {
    /// Fails when `SPACE_MASTER_KEY` is set but invalid.
    pub fn open(registry: CapsuleRegistry, nvram: NvramLog) -> Result<Self> {
        let inner = ModularPipeline::open(nvram, registry)?;
        Ok(Self { inner })
    }

    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        Self::open(registry, nvram).expect("open modular pipeline")
    }

    pub fn with_key_manager(
        registry: CapsuleRegistry,
        nvram: NvramLog,
        key_manager: KeyManager,
    ) -> Self {
        let inner = ModularPipeline::open_with_key(nvram, registry, key_manager)
            .expect("open modular pipeline with key manager");
        Self { inner }
    }

    /// Encrypted when a master key is configured.
    pub async fn default_policy(&self) -> Policy {
        self.inner.default_policy().await
    }

    /// Write with the pipeline's default policy.
    pub async fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        let policy = self.inner.default_policy().await;
        self.write_capsule_with_policy(data, &policy).await
    }

    /// Write from a reader one segment at a time. Returns the capsule and
    /// its size.
    pub async fn write_reader<Rd: AsyncRead + Send>(
        &self,
        reader: Rd,
        policy: &Policy,
    ) -> Result<(CapsuleId, u64)> {
        self.inner.write_reader(reader, policy).await
    }

    /// Read a capsule one segment at a time.
    pub async fn read_stream(&self, id: CapsuleId) -> Result<DataStream> {
        self.inner.read_stream(id).await
    }

    pub async fn write_capsule_with_policy(
        &self,
        data: &[u8],
        policy: &Policy,
    ) -> Result<CapsuleId> {
        policy.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
        self.inner.write_capsule(data, policy).await
    }

    pub async fn delete_capsule(&self, capsule_id: CapsuleId) -> Result<()> {
        self.inner.delete_capsule(capsule_id).await
    }

    pub async fn garbage_collect(&self) -> Result<usize> {
        self.inner.garbage_collect().await
    }

    pub async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        self.inner.read_capsule(id).await
    }

    pub async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_range(id, offset, len).await
    }
}

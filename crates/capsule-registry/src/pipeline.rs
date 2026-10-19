use anyhow::Result;
use common::{CapsuleId, Policy};
use encryption::KeyManager;
use nvram_sim::NvramLog;

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
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        let inner = ModularPipeline::open(nvram, registry).expect("open modular pipeline");
        Self { inner }
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

    pub async fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        self.write_capsule_with_policy(data, &Policy::default())
            .await
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

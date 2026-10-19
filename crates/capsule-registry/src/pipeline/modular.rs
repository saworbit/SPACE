use std::any::Any;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use common::{CapsuleId, Policy};
use encryption::KeyManager;
use nvram_sim::NvramLog;
use tokio::sync::Mutex as TokioMutex;

use crate::modular_pipeline::RegistryPipelineHandle;
use crate::pipeline::strategy::PipelineStrategy;
use crate::CapsuleRegistry;

pub struct ModularPipeline {
    handle: Arc<TokioMutex<RegistryPipelineHandle>>,
}

impl ModularPipeline {
    pub fn open(nvram: NvramLog, registry: CapsuleRegistry) -> Result<Self> {
        let handle = crate::modular_pipeline::registry_pipeline_from_log(nvram, registry)?;
        Ok(Self {
            handle: Arc::new(TokioMutex::new(handle)),
        })
    }

    pub fn open_with_key(
        nvram: NvramLog,
        registry: CapsuleRegistry,
        key_manager: KeyManager,
    ) -> Result<Self> {
        let key_manager = Arc::new(std::sync::Mutex::new(key_manager));
        let handle = crate::modular_pipeline::registry_pipeline_from_log_with_key(
            nvram,
            registry,
            key_manager,
        )?;
        Ok(Self {
            handle: Arc::new(TokioMutex::new(handle)),
        })
    }
}

#[async_trait]
impl PipelineStrategy for ModularPipeline {
    async fn write_capsule(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        let bytes = data.to_vec();
        let policy = policy.clone();
        let handle = Arc::clone(&self.handle);
        let mut guard = handle.lock().await;
        guard.write_capsule(&bytes, &policy).await
    }

    async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        self.handle.lock().await.read_capsule(id).await
    }

    async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.handle.lock().await.read_range(id, offset, len).await
    }

    async fn delete_capsule(&self, id: CapsuleId) -> Result<()> {
        self.handle.lock().await.delete_capsule(id).await
    }

    async fn garbage_collect(&self) -> Result<usize> {
        self.handle.lock().await.garbage_collect().await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

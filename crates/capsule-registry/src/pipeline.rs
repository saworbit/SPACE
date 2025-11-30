use anyhow::Result;
use common::{CapsuleId, Policy};
use futures::executor::block_on;
use nvram_sim::NvramLog;

use crate::CapsuleRegistry;

mod legacy;
#[cfg(feature = "modular_pipeline")]
mod modular;
pub mod strategy;

pub use legacy::LegacyPipeline;
#[cfg(feature = "pipeline_async")]
pub use legacy::PipelineConfig;
#[cfg(feature = "modular_pipeline")]
use modular::ModularPipeline;
use strategy::PipelineStrategy;

enum PipelineKind {
    Legacy(LegacyPipeline),
    #[cfg(feature = "modular_pipeline")]
    Modular(ModularPipeline),
}

/// Facade that selects a pipeline strategy (legacy vs modular) at runtime.
pub struct WritePipeline {
    strategy: PipelineKind,
}

impl WritePipeline {
    /// Build a pipeline, preferring the modular strategy when the feature is
    /// enabled (unless `SPACE_DISABLE_MODULAR_PIPELINE` is set). It can also be
    /// forced via `SPACE_USE_MODULAR=1`.
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        #[cfg(feature = "modular_pipeline")]
        {
            let modular_disabled = std::env::var("SPACE_DISABLE_MODULAR_PIPELINE").is_ok();
            let modular_forced = std::env::var("SPACE_USE_MODULAR").is_ok();
            if modular_forced || !modular_disabled {
                match ModularPipeline::try_new(nvram.clone(), registry.clone()) {
                    Ok(Some(strategy)) => {
                        return Self {
                            strategy: PipelineKind::Modular(strategy),
                        }
                    }
                    Ok(None) => {
                        tracing::warn!(
                            "SPACE_USE_MODULAR set but modular pipeline not available; falling back to legacy"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "modular pipeline init failed; falling back to legacy"
                        );
                    }
                }
            }
        }

        Self {
            strategy: PipelineKind::Legacy(LegacyPipeline::new(registry, nvram)),
        }
    }

    /// Build a pipeline with an explicit key manager (legacy path).
    pub fn with_key_manager(
        registry: CapsuleRegistry,
        nvram: NvramLog,
        key_manager: encryption::KeyManager,
    ) -> Self {
        #[cfg(feature = "modular_pipeline")]
        {
            let modular_disabled = std::env::var("SPACE_DISABLE_MODULAR_PIPELINE").is_ok();
            let modular_forced = std::env::var("SPACE_USE_MODULAR").is_ok();
            if modular_forced || !modular_disabled {
                if let Ok(Some(strategy)) =
                    ModularPipeline::try_new(nvram.clone(), registry.clone())
                {
                    return Self {
                        strategy: PipelineKind::Modular(strategy),
                    };
                }
            }
        }

        Self {
            strategy: PipelineKind::Legacy(LegacyPipeline::with_key_manager(
                registry,
                nvram,
                key_manager,
            )),
        }
    }

    fn strategy(&self) -> &dyn PipelineStrategy {
        match &self.strategy {
            PipelineKind::Legacy(inner) => inner,
            #[cfg(feature = "modular_pipeline")]
            PipelineKind::Modular(inner) => inner,
        }
    }

    #[allow(dead_code)]
    fn strategy_mut(&mut self) -> &mut dyn PipelineStrategy {
        match &mut self.strategy {
            PipelineKind::Legacy(inner) => inner,
            #[cfg(feature = "modular_pipeline")]
            PipelineKind::Modular(inner) => inner,
        }
    }

    fn as_legacy(&self) -> Option<&LegacyPipeline> {
        self.strategy().as_any().downcast_ref::<LegacyPipeline>()
    }

    #[allow(dead_code)]
    fn as_legacy_mut(&mut self) -> Option<&mut LegacyPipeline> {
        self.strategy_mut()
            .as_any_mut()
            .downcast_mut::<LegacyPipeline>()
    }

    pub fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        self.write_capsule_with_policy(data, &Policy::default())
    }

    pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        block_on(self.strategy().write_capsule(data, policy))
    }

    #[cfg(feature = "pipeline_async")]
    pub async fn write_capsule_with_policy_async(
        &self,
        data: &[u8],
        policy: &Policy,
    ) -> Result<CapsuleId> {
        self.strategy().write_capsule(data, policy).await
    }

    pub fn delete_capsule(&self, capsule_id: CapsuleId) -> Result<()> {
        block_on(self.strategy().delete_capsule(capsule_id))
    }

    pub fn garbage_collect(&self) -> Result<usize> {
        block_on(self.strategy().garbage_collect())
    }

    pub fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        block_on(self.strategy().read_capsule(id))
    }

    pub fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(legacy) = self.as_legacy() {
            return legacy.read_range(id, offset, len);
        }

        let full = self.read_capsule(id)?;
        if offset + len as u64 > full.len() as u64 {
            anyhow::bail!("Read beyond capsule boundary");
        }
        Ok(full[offset as usize..offset as usize + len].to_vec())
    }

    #[cfg(feature = "pipeline_async")]
    pub fn with_config(mut self, config: PipelineConfig) -> Self {
        if let Some(legacy) = self.as_legacy_mut() {
            legacy.set_config(config);
        }
        self
    }

    #[cfg(all(feature = "podms", feature = "pipeline_async"))]
    pub fn with_telemetry_channel(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<common::podms::Telemetry>,
    ) -> Self {
        if let Some(legacy) = self.as_legacy_mut() {
            legacy.set_telemetry_channel(tx);
        }
        self
    }

    #[cfg(all(feature = "podms", feature = "pipeline_async"))]
    pub fn with_mesh_node(
        mut self,
        mesh_node: std::sync::Arc<scaling::MeshNode<CapsuleRegistry>>,
    ) -> Self {
        if let Some(legacy) = self.as_legacy_mut() {
            legacy.set_mesh_node(mesh_node);
        }
        self
    }
}

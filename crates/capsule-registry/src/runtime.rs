use anyhow::{Context, Result};
use common::Policy;
use encryption::{EnvKeyProvider, FileKeyProvider, KeyManager, KeyProvider};
use futures::executor::block_on;
use nvram_sim::NvramLog;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::CapsuleRegistry;

/// Shared runtime handles for production deployments.
///
/// Provides ready-to-use registry, NVRAM log, and key-manager instances
/// constructed from environment configuration so scaling handlers can
/// operate on real data paths instead of mocks.
pub struct RuntimeHandles {
    pub registry: Arc<CapsuleRegistry>,
    pub nvram: Arc<RwLock<NvramLog>>,
    pub key_manager: Arc<RwLock<KeyManager>>,
}

impl RuntimeHandles {
    /// Build runtime handles from environment variables.
    ///
    /// - `SPACE_METADATA_PATH` (optional, default: "space.db")
    /// - `SPACE_NVRAM_PATH` (optional, default: "space.nvram")
    /// - `SPACE_MASTER_KEY_FILE` (optional, path to a key file)
    /// - `SPACE_MASTER_KEY` (optional, hex-encoded; dev fallback if unset)
    pub fn from_env() -> Result<Self> {
        let metadata_path =
            std::env::var("SPACE_METADATA_PATH").unwrap_or_else(|_| "space.db".to_string());
        let nvram_path =
            std::env::var("SPACE_NVRAM_PATH").unwrap_or_else(|_| "space.nvram".to_string());

        let registry = Arc::new(
            CapsuleRegistry::open(&metadata_path)
                .with_context(|| format!("opening registry at {}", metadata_path))?,
        );

        let nvram = Arc::new(RwLock::new(
            NvramLog::open(&nvram_path)
                .with_context(|| format!("opening nvram log at {}", nvram_path))?,
        ));

        let key_provider: Box<dyn KeyProvider> = match std::env::var("SPACE_MASTER_KEY_FILE") {
            Ok(path) if !path.trim().is_empty() => Box::new(FileKeyProvider::new(path)),
            _ => Box::new(EnvKeyProvider::default()),
        };

        let key_manager = Arc::new(RwLock::new(
            block_on(KeyManager::from_provider(&*key_provider))
                .context("initializing KeyManager from KeyProvider")?,
        ));

        Ok(Self {
            registry,
            nvram,
            key_manager,
        })
    }

    /// Convenience helper to build a scaling agent with production handles.
    #[cfg(feature = "podms")]
    pub fn build_scaling_agent<C: scaling::ContentStore + 'static>(
        &self,
        mesh_node: Arc<scaling::MeshNode<C>>,
        default_policy: Policy,
    ) -> scaling::agent::ScalingAgent<C> {
        scaling::agent::ScalingAgent::with_runtime(
            mesh_node,
            default_policy,
            self.registry.clone(),
            self.nvram.clone(),
            self.key_manager.clone(),
        )
    }
}

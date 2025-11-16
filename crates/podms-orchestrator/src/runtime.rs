//! Runtime helpers for the PODMS orchestrator.

use anyhow::{Context, Result};
use common::podms::Telemetry;
use mesh_core::GossipHandler;
use scaling::ContentStore;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

use crate::Orchestrator;

/// Runtime handle for interacting with a running orchestrator.
///
/// This provides a simplified interface for common operations like
/// emitting telemetry events, querying mesh state, and triggering actions.
pub struct OrchestratorRuntime<C: ContentStore> {
    /// Reference to the orchestrator
    orchestrator: Arc<Orchestrator<C>>,

    /// Telemetry sender for emitting events
    telemetry_tx: mpsc::UnboundedSender<Telemetry>,
}

impl<C: ContentStore + 'static> OrchestratorRuntime<C> {
    /// Create a new runtime handle from an orchestrator.
    pub fn new(orchestrator: Arc<Orchestrator<C>>) -> Self {
        let telemetry_tx = orchestrator.telemetry_sender();
        Self {
            orchestrator,
            telemetry_tx,
        }
    }

    /// Emit a telemetry event to trigger scaling actions.
    ///
    /// This is the primary way to drive autonomous operations. Events are
    /// processed by the policy compiler and may result in replication,
    /// migration, or other scaling actions.
    pub fn emit_telemetry(&self, event: Telemetry) -> Result<()> {
        self.telemetry_tx
            .send(event)
            .map_err(|e| anyhow::anyhow!("failed to send telemetry: {}", e))?;
        Ok(())
    }

    /// Emit a "new capsule" telemetry event.
    ///
    /// This triggers policy-based replication and placement decisions.
    pub fn notify_capsule_created(
        &self,
        capsule_id: common::CapsuleId,
        policy: common::Policy,
    ) -> Result<()> {
        let event = Telemetry::NewCapsule {
            id: capsule_id,
            policy,
            node_id: Some(self.orchestrator.node_id()),
        };

        self.emit_telemetry(event)
    }

    /// Emit a "heat spike" telemetry event for hot data migration.
    pub fn notify_heat_spike(
        &self,
        capsule_id: common::CapsuleId,
        accesses_per_min: u64,
    ) -> Result<()> {
        let event = Telemetry::HeatSpike {
            id: capsule_id,
            accesses_per_min,
            node_id: Some(self.orchestrator.node_id()),
        };

        self.emit_telemetry(event)
    }

    /// Emit a "capacity threshold" telemetry event for rebalancing.
    pub fn notify_capacity_threshold(
        &self,
        used_bytes: u64,
        total_bytes: u64,
        threshold_pct: f32,
    ) -> Result<()> {
        let event = Telemetry::CapacityThreshold {
            node_id: self.orchestrator.node_id(),
            used_bytes,
            total_bytes,
            threshold_pct: threshold_pct as f64, // Convert to expected type
        };

        self.emit_telemetry(event)
    }

    /// Emit a "node degraded" telemetry event for evacuation.
    pub fn notify_node_degraded(&self, reason: String) -> Result<()> {
        let event = Telemetry::NodeDegraded {
            node_id: self.orchestrator.node_id(),
            reason,
        };

        self.emit_telemetry(event)
    }

    /// Get the current gossip statistics.
    pub async fn gossip_stats(&self) -> Result<mesh_core::GossipStats> {
        self.orchestrator
            .gossip()
            .get_stats()
            .await
            .context("failed to get gossip stats")
    }

    /// Get the list of known peers.
    pub async fn peers(&self) -> Result<Vec<mesh_core::Peer>> {
        self.orchestrator
            .gossip()
            .get_peers()
            .await
            .context("failed to get peers")
    }

    /// Get the node ID.
    pub fn node_id(&self) -> common::podms::NodeId {
        self.orchestrator.node_id()
    }

    /// Broadcast a custom gossip message.
    pub async fn broadcast_gossip(
        &self,
        topic: &str,
        message: mesh_core::GossipMessage,
    ) -> Result<()> {
        self.orchestrator
            .gossip()
            .broadcast(topic, message)
            .await
            .context("failed to broadcast gossip message")
    }
}

/// Builder for creating and starting an orchestrator with optional runtime.
///
/// This provides a fluent API for configuring and launching the orchestrator.
#[allow(dead_code)] // Will be used once integration is complete
pub struct OrchestratorBuilder<C: ContentStore> {
    config: crate::OrchestratorConfig,
    content_store: Option<Arc<tokio::sync::RwLock<C>>>,
    catalog: Option<Arc<dyn common::traits::CapsuleCatalog + Send + Sync>>,
    nvram_log: Option<Arc<tokio::sync::RwLock<nvram_sim::NvramLog>>>,
    key_manager: Option<Arc<tokio::sync::RwLock<encryption::keymanager::KeyManager>>>,
}

#[allow(dead_code)] // Will be used once integration is complete
impl<C: ContentStore + 'static> OrchestratorBuilder<C> {
    /// Create a new builder with the specified configuration.
    pub fn new(config: crate::OrchestratorConfig) -> Self {
        Self {
            config,
            content_store: None,
            catalog: None,
            nvram_log: None,
            key_manager: None,
        }
    }

    /// Set the content store.
    pub fn with_content_store(mut self, store: Arc<tokio::sync::RwLock<C>>) -> Self {
        self.content_store = Some(store);
        self
    }

    /// Set the capsule catalog.
    pub fn with_catalog(
        mut self,
        catalog: Arc<dyn common::traits::CapsuleCatalog + Send + Sync>,
    ) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Set the NVRAM log.
    pub fn with_nvram_log(mut self, log: Arc<tokio::sync::RwLock<nvram_sim::NvramLog>>) -> Self {
        self.nvram_log = Some(log);
        self
    }

    /// Set the key manager.
    pub fn with_key_manager(
        mut self,
        km: Arc<tokio::sync::RwLock<encryption::keymanager::KeyManager>>,
    ) -> Self {
        self.key_manager = Some(km);
        self
    }

    /// Build and start the orchestrator, returning Arc-wrapped orchestrator and runtime handle.
    pub async fn build_and_start(self) -> Result<(Arc<Orchestrator<C>>, OrchestratorRuntime<C>)> {
        let content_store = self.content_store.context("content store not configured")?;
        let catalog = self.catalog.context("catalog not configured")?;
        let nvram_log = self.nvram_log.context("nvram log not configured")?;
        let key_manager = self.key_manager.context("key manager not configured")?;

        // Create orchestrator
        let mut orchestrator =
            Orchestrator::new(self.config, content_store, catalog, nvram_log, key_manager).await?;

        // Start orchestrator
        orchestrator.start().await?;

        info!("orchestrator started via builder");

        // Wrap in Arc for shared ownership
        let orchestrator_arc = Arc::new(orchestrator);

        // Create runtime handle
        let runtime = OrchestratorRuntime::new(orchestrator_arc.clone());

        Ok((orchestrator_arc, runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests require concrete ContentStore implementation
    // They will be added once we integrate with CapsuleRegistry
}

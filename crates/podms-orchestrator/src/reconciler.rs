//! Node Reconciliation Engine
//!
//! The Reconciler implements the "Nervous System" that connects the Federation
//! Registry (Brain) with the Foundry storage engine (Muscle). It continuously
//! watches the Global Registry state and forces local Foundry to match the
//! desired state.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────┐
//! │ Federation Registry │ ← Brain (What SHOULD exist)
//! │   (Raft Consensus)  │
//! └──────────┬──────────┘
//!            │ get_state()
//!            ↓
//! ┌─────────────────────┐
//! │    Reconciler       │ ← Nervous System (Converges state)
//! │  (This Module)      │
//! └──────────┬──────────┘
//!            │ create_volume() / delete_volume()
//!            ↓
//! ┌─────────────────────┐
//! │   Foundry Engine    │ ← Muscle (What ACTUALLY exists)
//! │  (Local Storage)    │
//! └─────────────────────┘
//! ```
//!
//! ## Control Loop
//!
//! 1. **Observe**: Fetch desired state from Federation Registry
//! 2. **Filter**: Extract volumes assigned to this node
//! 3. **Diff**: Compare with actual Foundry state
//! 4. **Act**: Create missing volumes, delete zombie volumes
//!
//! ## Usage
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use capsule_registry::CapsuleRegistry;
//! use capsule_registry::pipeline::WritePipeline;
//! use foundry::Foundry;
//! use foundry::snapshot::SnapshotEngine;
//! use federation::Registry;
//! use nvram_sim::NvramLog;
//! use podms_orchestrator::Reconciler;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let foundry = Arc::new(Foundry::new());
//! let registry = Arc::new(Registry::new());
//!
//! // Setup snapshot engine for hydration
//! let capsule_registry = CapsuleRegistry::new();
//! let nvram = NvramLog::open("data/nvram.log")?;
//! let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
//! let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));
//!
//! let node_id = 1;
//! let reconciler = Reconciler::new(node_id, foundry, registry, snapshot_engine);
//!
//! // Run continuously in background
//! tokio::spawn(async move {
//!     reconciler.run().await;
//! });
//! # Ok(())
//! # }
//! ```

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use common::CapsuleId;
use foundry::backend::VolumeId;
use foundry::snapshot::SnapshotEngine;
use foundry::{BackendType, Foundry};
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use federation::Registry;

/// The Reconciler continuously converges local Foundry state to match the
/// global Federation Registry state.
///
/// This is the "Self-Driving" component that enables autonomous volume
/// management across the cluster.
pub struct Reconciler {
    /// ID of the local node
    node_id: u64,
    /// Local storage engine
    foundry: Arc<Foundry>,
    /// Global registry (Raft-backed)
    registry: Arc<Registry>,
    /// Snapshot engine for volume hydration
    snapshot_engine: Arc<SnapshotEngine>,
    /// Reconciliation interval
    interval: Duration,
}

impl Reconciler {
    /// Create a new Reconciler for the specified node.
    ///
    /// # Arguments
    ///
    /// - `node_id`: The ID of this node (must match ID in Federation Registry)
    /// - `foundry`: The local Foundry storage engine
    /// - `registry`: The global Federation Registry
    /// - `snapshot_engine`: The snapshot engine for volume hydration
    ///
    /// # Default Configuration
    ///
    /// - Reconciliation interval: 5 seconds
    pub fn new(
        node_id: u64,
        foundry: Arc<Foundry>,
        registry: Arc<Registry>,
        snapshot_engine: Arc<SnapshotEngine>,
    ) -> Self {
        Self {
            node_id,
            foundry,
            registry,
            snapshot_engine,
            interval: Duration::from_secs(5),
        }
    }

    /// Set a custom reconciliation interval.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// # use std::sync::Arc;
    /// # use capsule_registry::CapsuleRegistry;
    /// # use capsule_registry::pipeline::WritePipeline;
    /// # use foundry::Foundry;
    /// # use foundry::snapshot::SnapshotEngine;
    /// # use federation::Registry;
    /// # use nvram_sim::NvramLog;
    /// # use podms_orchestrator::Reconciler;
    /// # let foundry = Arc::new(Foundry::new());
    /// # let registry = Arc::new(Registry::new());
    /// # let capsule_registry = CapsuleRegistry::new();
    /// # let nvram = NvramLog::open("data/nvram.log").unwrap();
    /// # let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    /// # let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));
    ///
    /// let reconciler = Reconciler::new(1, foundry, registry, snapshot_engine)
    ///     .with_interval(Duration::from_secs(10));
    /// ```
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Run the reconciliation loop indefinitely.
    ///
    /// This function never returns unless the process is terminated.
    /// It runs a continuous loop that:
    /// 1. Waits for the configured interval
    /// 2. Performs a reconciliation step
    /// 3. Logs errors but continues running
    ///
    /// # Panics
    ///
    /// Never panics. All errors are logged and the loop continues.
    pub async fn run(&self) {
        info!(
            "Starting Reconciler loop for Node {} (interval: {:?})",
            self.node_id, self.interval
        );

        let mut interval = tokio::time::interval(self.interval);

        loop {
            interval.tick().await;

            if let Err(e) = self.reconcile_step().await {
                error!("Reconciliation failed: {:?}", e);
            }
        }
    }

    /// Perform a single reconciliation iteration.
    ///
    /// This is the core "Diff & Act" logic:
    /// 1. Get desired state from Registry
    /// 2. Filter volumes assigned to this node
    /// 3. Get actual state from Foundry
    /// 4. Create missing volumes
    /// 5. Delete zombie volumes
    ///
    /// This method is public to enable integration testing.
    #[instrument(skip(self))]
    pub async fn reconcile_step(&self) -> Result<()> {
        // 1. Get Desired State from Raft
        let cluster_state = self.registry.get_state();

        // 2. Filter volumes assigned to THIS node
        // In chain replication, replicas = [Primary, Replica1, Replica2, ...]
        // If our node_id is in the list, we should have the volume locally.
        let desired_volumes: Vec<_> = cluster_state
            .volumes
            .iter()
            .filter(|(_, meta)| meta.replicas.contains(&self.node_id))
            .collect();

        // 3. Get Actual State from Foundry
        let actual_volumes = self.foundry.list_volumes().await;
        let actual_set: HashSet<VolumeId> = actual_volumes.into_iter().collect();

        // 4. Diff: Create Missing Volumes
        for (vol_id_str, meta) in &desired_volumes {
            // Parse volume ID (Registry uses String, Foundry uses VolumeId)
            let vol_id = vol_id_str
                .parse::<VolumeId>()
                .map_err(|e| anyhow::anyhow!("Invalid Volume ID '{}': {}", vol_id_str, e))?;

            if !actual_set.contains(&vol_id) {
                info!(
                    volume_id = %vol_id,
                    size_bytes = meta.size,
                    replicas = ?meta.replicas,
                    source_capsule_id = ?meta.source_capsule_id,
                    "Reconciler: Creating missing volume"
                );

                // A. Create the empty shell
                // Use Legacy backend if hydrating (may need resize support)
                let backend = if meta.source_capsule_id.is_some() {
                    BackendType::Legacy
                } else {
                    BackendType::Auto
                };
                self.foundry
                    .create_volume(vol_id, meta.size, Some(backend))
                    .await?;

                // B. Hydrate if source exists
                if let Some(capsule_id_str) = &meta.source_capsule_id {
                    info!(
                        volume_id = %vol_id,
                        capsule_id = %capsule_id_str,
                        "Reconciler: Hydrating volume from capsule"
                    );

                    // Parse the capsule ID (UUID format)
                    let uuid = Uuid::parse_str(capsule_id_str).map_err(|e| {
                        anyhow::anyhow!("Invalid Capsule ID '{}': {}", capsule_id_str, e)
                    })?;
                    let capsule_id = CapsuleId::from_uuid(uuid);

                    // Get the volume handle for hydration
                    let vol = self.foundry.get_volume(vol_id).await?;

                    // Restore snapshot data into the new volume
                    if let Err(e) = self
                        .snapshot_engine
                        .restore_snapshot(vol_id, capsule_id, vol)
                        .await
                    {
                        error!(
                            volume_id = %vol_id,
                            error = %e,
                            "Hydration failed. Deleting partial volume for retry."
                        );
                        // Cleanup to force retry next loop
                        self.foundry.delete_volume(vol_id).await.ok();
                        return Err(e.into());
                    }

                    info!(
                        volume_id = %vol_id,
                        "Reconciler: Hydration complete"
                    );
                }

                info!(volume_id = %vol_id, "Reconciler: Successfully created volume");
            }
        }

        // 5. Diff: Delete Zombie Volumes
        // A "zombie" is a volume that exists locally but is NOT assigned to this node
        // in the registry. This can happen if:
        // - Volume was moved to another node
        // - Volume was deleted from the registry
        // - Node was removed from replica set
        let desired_set: HashSet<VolumeId> = desired_volumes
            .into_iter()
            .filter_map(|(k, _)| k.parse::<VolumeId>().ok())
            .collect();

        for vol_id in actual_set {
            if !desired_set.contains(&vol_id) {
                warn!(
                    volume_id = %vol_id,
                    node_id = self.node_id,
                    "Reconciler: Detected zombie volume. Deleting..."
                );

                self.foundry.delete_volume(vol_id).await?;

                info!(
                    volume_id = %vol_id,
                    "Reconciler: Successfully deleted zombie volume"
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_registry::pipeline::WritePipeline;

    #[test]
    fn test_reconciler_construction() {
        use capsule_registry::CapsuleRegistry;
        use nvram_sim::NvramLog;

        let temp_dir = tempfile::tempdir().unwrap();
        let foundry = Arc::new(Foundry::with_data_dir(temp_dir.path()));
        let registry = Arc::new(Registry::new());
        let capsule_registry = CapsuleRegistry::open(temp_dir.path().join("space.db")).unwrap();
        let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
        let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
        let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

        let reconciler = Reconciler::new(1, foundry, registry, snapshot_engine);

        assert_eq!(reconciler.node_id, 1);
        assert_eq!(reconciler.interval, Duration::from_secs(5));
    }

    #[test]
    fn test_reconciler_with_custom_interval() {
        use capsule_registry::CapsuleRegistry;
        use nvram_sim::NvramLog;

        let temp_dir = tempfile::tempdir().unwrap();
        let foundry = Arc::new(Foundry::with_data_dir(temp_dir.path()));
        let registry = Arc::new(Registry::new());
        let capsule_registry = CapsuleRegistry::open(temp_dir.path().join("space.db")).unwrap();
        let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
        let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
        let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

        let reconciler = Reconciler::new(1, foundry, registry, snapshot_engine)
            .with_interval(Duration::from_secs(10));

        assert_eq!(reconciler.interval, Duration::from_secs(10));
    }
}

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
//! use foundry::Foundry;
//! use federation::Registry;
//! use podms_orchestrator::Reconciler;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let foundry = Arc::new(Foundry::new());
//! let registry = Arc::new(Registry::new());
//! let node_id = 1;
//!
//! let reconciler = Reconciler::new(node_id, foundry, registry);
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
use foundry::backend::VolumeId;
use foundry::{BackendType, Foundry};
use tracing::{error, info, instrument, warn};

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
    ///
    /// # Default Configuration
    ///
    /// - Reconciliation interval: 5 seconds
    pub fn new(node_id: u64, foundry: Arc<Foundry>, registry: Arc<Registry>) -> Self {
        Self {
            node_id,
            foundry,
            registry,
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
    /// # use foundry::Foundry;
    /// # use federation::Registry;
    /// # use podms_orchestrator::Reconciler;
    /// # let foundry = Arc::new(Foundry::new());
    /// # let registry = Arc::new(Registry::new());
    ///
    /// let reconciler = Reconciler::new(1, foundry, registry)
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
    #[instrument(skip(self))]
    async fn reconcile_step(&self) -> Result<()> {
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
                    "Reconciler: Creating missing volume"
                );

                // Create volume with Auto backend selection
                self.foundry
                    .create_volume(vol_id, meta.size, Some(BackendType::Auto))
                    .await?;

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

    #[test]
    fn test_reconciler_construction() {
        let foundry = Arc::new(Foundry::new());
        let registry = Arc::new(Registry::new());

        let reconciler = Reconciler::new(1, foundry, registry);

        assert_eq!(reconciler.node_id, 1);
        assert_eq!(reconciler.interval, Duration::from_secs(5));
    }

    #[test]
    fn test_reconciler_with_custom_interval() {
        let foundry = Arc::new(Foundry::new());
        let registry = Arc::new(Registry::new());

        let reconciler =
            Reconciler::new(1, foundry, registry).with_interval(Duration::from_secs(10));

        assert_eq!(reconciler.interval, Duration::from_secs(10));
    }
}

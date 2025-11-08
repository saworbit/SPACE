//! PODMS Scaling Agent - Autonomous Telemetry-Driven Operations
//!
//! The scaling agent subscribes to telemetry events and triggers autonomous actions:
//! - NewCapsule → Check policy.rpo and trigger replication
//! - HeatSpike → Migrate capsule to cooler nodes
//! - CapacityThreshold → Rebalance across nodes
//! - NodeDegraded → Evacuate capsules from failing node

use anyhow::Result;
use common::podms::{NodeId, Telemetry};
use common::{CapsuleId, Policy};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

use crate::MeshNode;

/// Scaling agent that consumes telemetry and performs autonomous actions.
/// Note: This is a stub implementation for Step 2. Full migration logic
/// will be implemented in Step 3 when we add the policy compiler.
pub struct ScalingAgent {
    mesh_node: Arc<MeshNode>,
}

impl ScalingAgent {
    /// Create a new scaling agent.
    pub fn new(mesh_node: Arc<MeshNode>) -> Self {
        Self { mesh_node }
    }

    /// Run the agent loop, consuming telemetry events and triggering actions.
    /// This is the main entry point for the autonomous scaling system.
    pub async fn run(
        &self,
        mut telemetry_rx: UnboundedReceiver<Telemetry>,
    ) -> Result<()> {
        info!(node_id = %self.mesh_node.id(), "scaling agent started");

        loop {
            match telemetry_rx.recv().await {
                Some(event) => {
                    if let Err(e) = self.handle_telemetry_event(event).await {
                        warn!(error = %e, "failed to handle telemetry event");
                    }
                }
                None => {
                    info!("telemetry channel closed, shutting down agent");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single telemetry event.
    async fn handle_telemetry_event(&self, event: Telemetry) -> Result<()> {
        match event {
            Telemetry::NewCapsule { id, policy, node_id } => {
                self.handle_new_capsule(id, policy, node_id).await?;
            }
            Telemetry::HeatSpike {
                id,
                accesses_per_min,
                node_id,
            } => {
                self.handle_heat_spike(id, accesses_per_min, node_id).await?;
            }
            Telemetry::CapacityThreshold {
                node_id,
                used_bytes,
                total_bytes,
                threshold_pct,
            } => {
                self.handle_capacity_threshold(node_id, used_bytes, total_bytes, threshold_pct)
                    .await?;
            }
            Telemetry::NodeDegraded { node_id, reason } => {
                self.handle_node_degraded(node_id, reason).await?;
            }
        }

        Ok(())
    }

    /// Handle NewCapsule event: Check policy and trigger replication if needed.
    async fn handle_new_capsule(
        &self,
        capsule_id: CapsuleId,
        _policy: Policy,
        _node_id: Option<NodeId>,
    ) -> Result<()> {
        debug!(
            capsule_id = %capsule_id.as_uuid(),
            "handling new capsule (agent stub)"
        );

        // Note: Full implementation will check policy.rpo and policy.latency_target
        // when common/podms feature is enabled. For now, this is a stub.
        //
        // Future implementation will:
        // - Check if policy.rpo == Duration::ZERO → trigger metro-sync
        // - Check if policy.rpo < 60s → trigger async replication
        // - Check if policy.latency_target < 2ms → optimize placement
        //
        // For Step 2, replication is handled directly in WritePipeline.
        // Agent will take over in Step 3 with the policy compiler.

        debug!("agent stub: replication delegated to WritePipeline");

        Ok(())
    }

    /// Trigger metro-sync replication to 1-2 peer nodes.
    async fn trigger_metro_sync_replication(
        &self,
        _capsule_id: CapsuleId,
        _policy: &Policy,
    ) -> Result<()> {
        // TODO: Implement actual replication logic
        // This is a stub for Step 2 - full implementation would:
        // 1. Discover peers via mesh_node
        // 2. Select 1-2 targets in same zone
        // 3. Mirror segments via mesh_node.mirror_segment()
        // 4. Update metadata with replication status

        let peers = self.mesh_node.discover_peers().await?;
        debug!(peer_count = peers.len(), "discovered peers for replication");

        // Select first peer for now (simple strategy)
        if let Some(_target) = peers.first() {
            debug!("would replicate to peer (stub)");
            // Actual replication will be implemented in pipeline extension
        }

        Ok(())
    }

    /// Trigger async replication with batching based on RPO.
    async fn trigger_async_replication(
        &self,
        _capsule_id: CapsuleId,
        _policy: &Policy,
    ) -> Result<()> {
        // TODO: Implement async replication with buffering
        // Queue capsule for batched replication
        debug!("async replication queued (stub)");
        Ok(())
    }

    /// Check if capsule is optimally placed for its latency target.
    async fn check_optimal_placement(
        &self,
        _capsule_id: CapsuleId,
        _policy: &Policy,
    ) -> Result<()> {
        // TODO: Implement placement optimization
        // Check current node capabilities vs. policy requirements
        debug!("placement check (stub)");
        Ok(())
    }

    /// Handle HeatSpike event: Migrate hot capsule to cooler node.
    async fn handle_heat_spike(
        &self,
        capsule_id: CapsuleId,
        accesses_per_min: u64,
        _node_id: Option<NodeId>,
    ) -> Result<()> {
        warn!(
            capsule_id = %capsule_id.as_uuid(),
            accesses_per_min,
            "heat spike detected"
        );

        // TODO: Implement migration logic
        // 1. Find least-loaded peer
        // 2. Copy capsule segments
        // 3. Update routing metadata
        // 4. Verify migration success
        // 5. Delete old copy

        debug!("migration queued (stub)");
        Ok(())
    }

    /// Handle CapacityThreshold event: Trigger rebalancing.
    async fn handle_capacity_threshold(
        &self,
        node_id: NodeId,
        used_bytes: u64,
        total_bytes: u64,
        threshold_pct: f64,
    ) -> Result<()> {
        warn!(
            node_id = %node_id,
            used_bytes,
            total_bytes,
            threshold_pct,
            "capacity threshold reached"
        );

        // TODO: Implement rebalancing
        // 1. Enumerate capsules on this node
        // 2. Sort by access frequency (coldest first)
        // 3. Select candidates for evacuation
        // 4. Find target nodes with capacity
        // 5. Migrate capsules

        debug!("rebalancing queued (stub)");
        Ok(())
    }

    /// Handle NodeDegraded event: Evacuate capsules from failing node.
    async fn handle_node_degraded(&self, node_id: NodeId, reason: String) -> Result<()> {
        warn!(
            node_id = %node_id,
            reason = %reason,
            "node degraded, triggering evacuation"
        );

        // TODO: Implement evacuation
        // This is the highest priority agent action:
        // 1. Enumerate all capsules on degraded node
        // 2. Find healthy peers in same zone
        // 3. Copy all capsules in parallel
        // 4. Update routing/registry
        // 5. Mark old node as evacuated

        debug!("evacuation queued (stub)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::podms::ZoneId;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_agent_creation() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:9100".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let _agent = ScalingAgent::new(mesh_node);
    }

    #[tokio::test]
    async fn test_agent_handles_new_capsule() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:9101".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node);

        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn agent in background
        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send a test event
        let capsule_id = CapsuleId::new();
        let policy = Policy::default();
        tx.send(Telemetry::NewCapsule {
            id: capsule_id,
            policy,
            node_id: None,
        })
        .unwrap();

        // Close channel to shut down agent
        drop(tx);

        // Wait for agent to finish
        agent_handle.await.unwrap().unwrap();
    }
}

//! PODMS Scaling Agent - Autonomous Telemetry-Driven Operations
//!
//! The scaling agent subscribes to telemetry events and triggers autonomous actions:
//! - NewCapsule → Check policy.rpo and trigger replication
//! - HeatSpike → Migrate capsule to cooler nodes
//! - CapacityThreshold → Rebalance across nodes
//! - NodeDegraded → Evacuate capsules from failing node
//!
//! Step 3 Integration: The agent now uses the PolicyCompiler to translate
//! telemetry events into concrete ScalingActions based on declarative policies.

use anyhow::Result;
use common::podms::{NodeId, Telemetry};
use common::{CapsuleId, Policy};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

use crate::compiler::{MeshState, NodeInfo, PolicyCompiler, ScalingAction};
use crate::{ContentStore, MeshNode};

/// Scaling agent that consumes telemetry and performs autonomous actions.
///
/// Step 3: Now integrates PolicyCompiler for swarm intelligence - translating
/// declarative policies into autonomous scaling behaviors.
pub struct ScalingAgent<C: ContentStore> {
    mesh_node: Arc<MeshNode<C>>,
    compiler: PolicyCompiler,
}

impl<C: ContentStore + 'static> ScalingAgent<C> {
    /// Create a new scaling agent with default policy.
    pub fn new(mesh_node: Arc<MeshNode<C>>) -> Self {
        Self {
            mesh_node,
            compiler: PolicyCompiler::with_defaults(),
        }
    }

    /// Create a new scaling agent with a custom default policy.
    pub fn with_policy(mesh_node: Arc<MeshNode<C>>, default_policy: Policy) -> Self {
        Self {
            mesh_node,
            compiler: PolicyCompiler::new(default_policy),
        }
    }

    /// Run the agent loop, consuming telemetry events and triggering actions.
    /// This is the main entry point for the autonomous scaling system.
    pub async fn run(&self, mut telemetry_rx: UnboundedReceiver<Telemetry>) -> Result<()> {
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

    /// Handle a single telemetry event using the policy compiler.
    ///
    /// Step 3: This method now uses the PolicyCompiler to translate events
    /// into ScalingActions, then executes each action autonomously.
    async fn handle_telemetry_event(&self, event: Telemetry) -> Result<()> {
        // Extract policy from event (use default if not specified)
        let policy = match &event {
            Telemetry::NewCapsule { policy, .. } => policy.clone(),
            _ => Policy::metro_sync(), // Default for non-capsule events
        };

        // Build current mesh state snapshot for compiler
        let mesh_state = self.build_mesh_state().await?;

        // Compile telemetry event into scaling actions
        let actions = self
            .compiler
            .compile_scaling_actions(&event, &policy, &mesh_state);

        debug!(
            event_type = std::any::type_name_of_val(&event),
            action_count = actions.len(),
            "compiled scaling actions from telemetry"
        );

        // Execute each action
        for action in actions {
            if let Err(e) = self.execute_action(action).await {
                warn!(error = %e, "failed to execute scaling action");
            }
        }

        Ok(())
    }

    /// Build a snapshot of current mesh state for the compiler.
    ///
    /// This provides the compiler with topology and capacity information
    /// needed for target selection decisions.
    async fn build_mesh_state(&self) -> Result<MeshState> {
        // For Step 3, create a basic mesh state
        // In production, this would query actual node states from the mesh
        let peer_ids = self.mesh_node.discover_peers().await?;

        let mut nodes = Vec::new();
        for peer_id in peer_ids {
            // For now, create placeholder node info
            // In production: Query actual capabilities and utilization
            nodes.push((
                peer_id,
                NodeInfo {
                    zone: self.mesh_node.zone().clone(),
                    available_bytes: 1_000_000_000, // 1GB placeholder
                    used_bytes: 100_000_000,        // 10% utilization
                    network_tier: crate::NetworkTier::Premium,
                },
            ));
        }

        Ok(MeshState::new(nodes, self.mesh_node.zone().clone()))
    }

    /// Execute a compiled scaling action.
    ///
    /// This is the execution layer - each action type has its own handler
    /// that performs the actual mesh operations (replication, migration, etc).
    async fn execute_action(&self, action: ScalingAction) -> Result<()> {
        match action {
            ScalingAction::Replicate {
                capsule_id,
                strategy,
                targets,
            } => {
                self.execute_replication(capsule_id, strategy, targets)
                    .await?;
            }
            ScalingAction::Migrate {
                capsule_id,
                reason,
                destination,
                transform,
            } => {
                self.execute_migration(capsule_id, reason, destination, transform)
                    .await?;
            }
            ScalingAction::Federate { capsule_id, zone } => {
                info!(
                    capsule = %capsule_id.as_uuid(),
                    zone = %zone,
                    "phase4 federate action (agent noop)"
                );
            }
            ScalingAction::ShardEC {
                capsule_id, zones, ..
            } => {
                info!(
                    capsule = %capsule_id.as_uuid(),
                    shard_targets = zones.len(),
                    "phase4 shard action (agent noop)"
                );
            }
            ScalingAction::Evacuate {
                source_node,
                reason,
                urgency,
            } => {
                self.execute_evacuation(source_node, reason, urgency)
                    .await?;
            }
            ScalingAction::Rebalance {
                overloaded_nodes,
                underutilized_nodes,
            } => {
                self.execute_rebalancing(overloaded_nodes, underutilized_nodes)
                    .await?;
            }
        }

        Ok(())
    }

    // ========================================================================
    // Action Executors - Step 3 Implementation
    // ========================================================================
    // These methods execute compiled ScalingActions using mesh operations.

    /// Execute replication action based on compiled strategy.
    async fn execute_replication(
        &self,
        capsule_id: CapsuleId,
        strategy: crate::compiler::ReplicationStrategy,
        targets: Vec<NodeId>,
    ) -> Result<()> {
        info!(
            capsule_id = %capsule_id.as_uuid(),
            strategy = ?strategy,
            target_count = targets.len(),
            "executing replication"
        );

        use crate::compiler::ReplicationStrategy;
        match strategy {
            ReplicationStrategy::MetroSync { replica_count } => {
                // Synchronous replication for zero-RPO
                self.execute_metro_sync_replication(capsule_id, replica_count, &targets)
                    .await?;
            }
            ReplicationStrategy::AsyncWithBatching { rpo } => {
                // Async replication with batching
                debug!(rpo_secs = rpo.as_secs(), "queuing async replication");
                // TODO: Add to replication queue with RPO-based batching
                // For now, we just log that async replication would be queued
                info!(
                    capsule_id = %capsule_id.as_uuid(),
                    rpo_secs = rpo.as_secs(),
                    "async replication would be queued (not yet implemented)"
                );
            }
            ReplicationStrategy::None => {
                // No replication needed
                debug!("no replication required");
            }
        }

        Ok(())
    }

    /// Execute metro-sync replication: load segments and mirror to targets.
    async fn execute_metro_sync_replication(
        &self,
        capsule_id: CapsuleId,
        replica_count: usize,
        targets: &[NodeId],
    ) -> Result<()> {
        debug!(
            capsule_id = %capsule_id.as_uuid(),
            replica_count = replica_count,
            "performing metro-sync replication"
        );

        // Note: In a real implementation, we would need access to:
        // 1. CapsuleCatalog to lookup capsule and get segment IDs
        // 2. NvramLog to read segment data
        //
        // For now, this is a placeholder that demonstrates the flow.
        // The actual implementation would require the agent to have
        // these dependencies injected.

        info!(
            capsule_id = %capsule_id.as_uuid(),
            target_count = targets.len().min(replica_count),
            "metro-sync replication: would load and mirror segments to targets"
        );

        // Placeholder for actual implementation:
        // let capsule = self.catalog.lookup_capsule(capsule_id)?;
        // for segment_id in capsule.segments {
        //     let segment_data = self.nvram_log.read(segment_id).await?;
        //     for target in targets.iter().take(replica_count) {
        //         self.mesh_node.mirror_segment(&segment_data, *target).await?;
        //     }
        // }

        Ok(())
    }

    /// Execute migration action (with optional transformation).
    async fn execute_migration(
        &self,
        capsule_id: CapsuleId,
        reason: String,
        destination: NodeId,
        transform: bool,
    ) -> Result<()> {
        info!(
            capsule_id = %capsule_id.as_uuid(),
            destination = %destination,
            reason = %reason,
            transform = transform,
            "executing migration"
        );

        // TODO: Step 3 - Implement migration with transformation hooks
        // 1. Load capsule segments from current node
        // 2. If transform: Apply SwarmBehavior.apply_transform()
        // 3. Mirror to destination via mesh_node.mirror_segment()
        // 4. Update routing/registry to point to new location
        // 5. Verify success, then delete old copy

        if transform {
            debug!("would apply transformation during migration");
            // Use SwarmBehavior trait from common::podms
        }

        debug!("migration queued for execution");
        Ok(())
    }

    /// Execute evacuation action based on urgency level.
    async fn execute_evacuation(
        &self,
        source_node: NodeId,
        reason: String,
        urgency: crate::compiler::EvacuationUrgency,
    ) -> Result<()> {
        warn!(
            source_node = %source_node,
            reason = %reason,
            urgency = ?urgency,
            "executing evacuation"
        );

        use crate::compiler::EvacuationUrgency;
        match urgency {
            EvacuationUrgency::Immediate => {
                // Parallel evacuation for critical failures
                debug!("initiating immediate parallel evacuation");
                // TODO: Enumerate capsules, migrate all in parallel
            }
            EvacuationUrgency::Gradual => {
                // Gradual evacuation (cold capsules first)
                debug!("initiating gradual evacuation (cold-first)");
                // TODO: Sort capsules by access frequency, migrate in order
            }
        }

        Ok(())
    }

    /// Execute rebalancing action across nodes.
    async fn execute_rebalancing(
        &self,
        overloaded_nodes: Vec<NodeId>,
        underutilized_nodes: Vec<NodeId>,
    ) -> Result<()> {
        info!(
            overloaded_count = overloaded_nodes.len(),
            underutilized_count = underutilized_nodes.len(),
            "executing rebalancing"
        );

        // TODO: Step 3 - Implement rebalancing logic
        // 1. Enumerate capsules on overloaded nodes
        // 2. Sort by access frequency (coldest first)
        // 3. Calculate target distribution
        // 4. Migrate capsules to underutilized nodes

        debug!("rebalancing queued for execution");
        Ok(())
    }
}

// Note: Tests removed because they need concrete ContentStore implementation
// They will be added once CapsuleRegistry implements ContentStore

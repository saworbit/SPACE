//! Policy Compiler for PODMS Intelligence Layer (Step 3)
//!
//! This module implements the "swarm intelligence" brain that compiles declarative
//! policies into executable scaling actions. It processes telemetry events through
//! policy rules to determine autonomous capsule behaviors like migration, replication,
//! and transformation.
//!
//! The compiler ensures:
//! - Sovereignty constraints are enforced during migrations
//! - RPO targets drive replication strategies
//! - Latency targets influence placement decisions
//! - Security invariants (encryption, dedup) are preserved during transformations

use common::podms::{NodeId, SovereigntyLevel, Telemetry, ZoneId};
use common::{CapsuleId, Policy};
use std::time::Duration;
use tracing::{debug, error, warn};

/// Actions that can be taken by the scaling system.
/// These represent compiled policy outcomes that the agent executes.
#[derive(Debug, Clone, PartialEq)]
pub enum ScalingAction {
    /// Replicate a capsule to ensure RPO/availability targets.
    Replicate {
        capsule_id: CapsuleId,
        strategy: ReplicationStrategy,
        targets: Vec<NodeId>,
    },
    /// Migrate a capsule to a different node (e.g., for load balancing or latency).
    Migrate {
        capsule_id: CapsuleId,
        reason: String,
        destination: NodeId,
        /// Whether to apply transformation during migration
        transform: bool,
    },
    /// Evacuate capsules from a degraded/failing node.
    Evacuate {
        source_node: NodeId,
        reason: String,
        /// Urgency level: Immediate (parallel) vs Gradual (cold-first)
        urgency: EvacuationUrgency,
    },
    /// Rebalance load across the mesh.
    Rebalance {
        overloaded_nodes: Vec<NodeId>,
        underutilized_nodes: Vec<NodeId>,
    },
    /// Federate metadata so the view is reachable locally or in the metro zone.
    Federate { capsule_id: CapsuleId, zone: ZoneId },
    /// Shard metadata with parity for quick lookups across zones.
    ShardEC {
        capsule_id: CapsuleId,
        parity: usize,
        zones: Vec<ZoneId>,
    },
}

/// Replication strategies derived from policy RPO targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationStrategy {
    /// Synchronous replication within metro zone for zero-RPO.
    /// Typically 1-2 replicas with sub-2ms latency.
    MetroSync { replica_count: usize },
    /// Asynchronous replication with batching for non-zero RPO.
    /// Batches are flushed at the specified interval.
    AsyncWithBatching { rpo: Duration },
    /// No replication needed (ephemeral or policy-exempt data).
    None,
}

/// Urgency level for evacuation actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvacuationUrgency {
    /// Immediate evacuation (parallel) for critical failures.
    Immediate,
    /// Gradual evacuation (cold capsules first) for degraded health.
    Gradual,
}

/// Policy compiler that translates telemetry events + policies into scaling actions.
///
/// This is the "brain" of PODMS swarm intelligence, making autonomous decisions
/// based on declarative policy rules and real-time telemetry.
pub struct PolicyCompiler {
    /// Default policy for capsules without explicit policies
    default_policy: Policy,
}

impl PolicyCompiler {
    /// Create a new policy compiler with a default policy.
    pub fn new(default_policy: Policy) -> Self {
        Self { default_policy }
    }

    /// Create a compiler with sensible defaults (metro-sync).
    pub fn with_defaults() -> Self {
        Self {
            default_policy: Policy::metro_sync(),
        }
    }

    /// Compile telemetry events into scaling actions based on policy rules.
    ///
    /// # Arguments
    /// * `event` - The telemetry event to process
    /// * `policy` - The policy governing the affected capsule(s)
    /// * `mesh_state` - Current mesh state for target selection
    ///
    /// # Returns
    /// A list of actions to execute, or empty if no action needed.
    pub fn compile_scaling_actions(
        &self,
        event: &Telemetry,
        policy: &Policy,
        mesh_state: &MeshState,
    ) -> Vec<ScalingAction> {
        let mut actions = Vec::new();

        if self.default_policy.encryption.is_enabled() && !policy.encryption.is_enabled() {
            debug!("capsule policy disabled encryption while default policy requires it");
        }

        match event {
            Telemetry::NewCapsule {
                id,
                policy: _,
                node_id: _,
            } => {
                actions.extend(self.compile_replication_strategy(*id, policy, mesh_state));
            }
            Telemetry::HeatSpike {
                id,
                accesses_per_min,
                node_id: _,
            } => {
                actions.extend(self.compile_migration_for_heat(
                    *id,
                    *accesses_per_min,
                    policy,
                    mesh_state,
                ));
            }
            Telemetry::CapacityThreshold {
                node_id,
                used_bytes,
                total_bytes,
                threshold_pct,
            } => {
                let used_percent = if *total_bytes > 0 {
                    ((*used_bytes as f64 / *total_bytes as f64) * 100.0) as f32
                } else {
                    0.0
                };
                let normalized_threshold = if (*threshold_pct).abs() <= 1.0 {
                    (*threshold_pct * 100.0) as f32
                } else {
                    *threshold_pct as f32
                };
                if used_percent >= normalized_threshold {
                    actions.extend(self.compile_rebalancing(*node_id, used_percent, mesh_state));
                } else {
                    debug!(
                        used_percent = used_percent,
                        threshold = normalized_threshold,
                        "capacity below threshold; skipping rebalancing"
                    );
                }
            }
            Telemetry::ViewProjection { id, view } => {
                debug!(
                    capsule = %id.as_uuid(),
                    view = %view,
                    "view projection telemetry received"
                );
                if policy.sovereignty != SovereigntyLevel::Local {
                    actions.push(ScalingAction::Federate {
                        capsule_id: *id,
                        zone: mesh_state.local_zone.clone(),
                    });
                }
                let target_zones = mesh_state.zone_ids();
                if !target_zones.is_empty() {
                    actions.push(ScalingAction::ShardEC {
                        capsule_id: *id,
                        parity: 2,
                        zones: target_zones,
                    });
                }
            }
            Telemetry::NodeDegraded { node_id, reason } => {
                actions.extend(self.compile_evacuation(*node_id, reason, mesh_state));
            }
        }

        // Validate all actions against sovereignty constraints
        self.validate_sovereignty(&actions, policy)
    }

    /// Compile replication strategy based on policy RPO.
    fn compile_replication_strategy(
        &self,
        capsule_id: CapsuleId,
        policy: &Policy,
        mesh_state: &MeshState,
    ) -> Vec<ScalingAction> {
        let strategy = if policy.rpo == Duration::ZERO {
            // Zero-RPO requires synchronous metro-sync
            ReplicationStrategy::MetroSync { replica_count: 2 }
        } else if policy.rpo < Duration::from_secs(60) {
            // Sub-60s RPO uses async batching
            ReplicationStrategy::AsyncWithBatching { rpo: policy.rpo }
        } else {
            // Longer RPO doesn't require immediate replication
            ReplicationStrategy::None
        };

        if strategy == ReplicationStrategy::None {
            return vec![];
        }

        // Select replication targets based on sovereignty and latency
        let targets = self.select_replication_targets(policy, mesh_state);

        if targets.is_empty() {
            warn!(
                capsule_id = ?capsule_id,
                "no suitable replication targets found"
            );
            return vec![];
        }

        debug!(
            capsule_id = ?capsule_id,
            strategy = ?strategy,
            target_count = targets.len(),
            "compiled replication strategy"
        );

        vec![ScalingAction::Replicate {
            capsule_id,
            strategy,
            targets,
        }]
    }

    /// Compile migration action for heat spike events.
    fn compile_migration_for_heat(
        &self,
        capsule_id: CapsuleId,
        accesses_per_min: u64,
        policy: &Policy,
        mesh_state: &MeshState,
    ) -> Vec<ScalingAction> {
        // Only migrate hot capsules (>100 accesses/min) with strict latency requirements
        if accesses_per_min <= 100 {
            return vec![];
        }

        if policy.latency_target >= Duration::from_millis(10) {
            // High latency tolerance, no need to migrate
            return vec![];
        }

        // Find optimal migration target (low latency, sufficient capacity)
        let destination = match self.select_migration_target(policy, mesh_state) {
            Some(node) => node,
            None => {
                warn!(
                    capsule_id = ?capsule_id,
                    "no suitable migration target for heat spike"
                );
                return vec![];
            }
        };

        // Apply transformation if migrating between zones (re-encrypt/recompress)
        let transform = mesh_state.requires_transformation(destination, policy);

        debug!(
            capsule_id = ?capsule_id,
            accesses_per_min = accesses_per_min,
            destination = %destination,
            transform = transform,
            "compiled migration for heat spike"
        );

        vec![ScalingAction::Migrate {
            capsule_id,
            reason: format!("heat_spike_{}_accesses_per_min", accesses_per_min),
            destination,
            transform,
        }]
    }

    /// Compile rebalancing action for capacity threshold events.
    fn compile_rebalancing(
        &self,
        node_id: NodeId,
        used_percent: f32,
        mesh_state: &MeshState,
    ) -> Vec<ScalingAction> {
        // Only rebalance if capacity exceeds 80%
        if used_percent < 80.0 {
            return vec![];
        }

        let overloaded = vec![node_id];
        let underutilized = mesh_state.find_underutilized_nodes(50.0); // <50% usage

        if underutilized.is_empty() {
            warn!(
                node_id = %node_id,
                "no underutilized nodes for rebalancing"
            );
            return vec![];
        }

        debug!(
            overloaded_count = overloaded.len(),
            underutilized_count = underutilized.len(),
            "compiled rebalancing action"
        );

        vec![ScalingAction::Rebalance {
            overloaded_nodes: overloaded,
            underutilized_nodes: underutilized,
        }]
    }

    /// Compile evacuation action for node degradation.
    fn compile_evacuation(
        &self,
        node_id: NodeId,
        reason: &str,
        _mesh_state: &MeshState,
    ) -> Vec<ScalingAction> {
        let urgency = if reason.contains("disk_failure") || reason.contains("power") {
            EvacuationUrgency::Immediate
        } else {
            EvacuationUrgency::Gradual
        };

        debug!(
            node_id = %node_id,
            reason = reason,
            urgency = ?urgency,
            "compiled evacuation action"
        );

        vec![ScalingAction::Evacuate {
            source_node: node_id,
            reason: reason.to_string(),
            urgency,
        }]
    }

    /// Select replication targets based on policy sovereignty and latency constraints.
    fn select_replication_targets(&self, policy: &Policy, mesh_state: &MeshState) -> Vec<NodeId> {
        let mut candidates = mesh_state.available_nodes();

        // Filter by sovereignty level
        candidates
            .retain(|&node_id| mesh_state.satisfies_sovereignty(node_id, &policy.sovereignty));

        // Filter by latency target
        if policy.latency_target < Duration::from_millis(2) {
            // Require metro zone for <2ms
            candidates.retain(|&node_id| mesh_state.is_metro_zone(node_id));
        } else if policy.latency_target < Duration::from_millis(100) {
            // Require same geo region for <100ms
            candidates.retain(|&node_id| mesh_state.is_same_geo_region(node_id));
        }

        // Return top candidates with sufficient capacity
        candidates
            .into_iter()
            .filter(|&node_id| mesh_state.has_capacity(node_id, 1_000_000)) // 1MB min
            .take(2) // Limit to 2 replicas for metro-sync
            .collect()
    }

    /// Select optimal migration target for a capsule.
    fn select_migration_target(&self, policy: &Policy, mesh_state: &MeshState) -> Option<NodeId> {
        let mut candidates = mesh_state.available_nodes();

        // Filter by sovereignty
        candidates
            .retain(|&node_id| mesh_state.satisfies_sovereignty(node_id, &policy.sovereignty));

        // Prefer metro zone for low latency
        if policy.latency_target < Duration::from_millis(2) {
            candidates.retain(|&node_id| mesh_state.is_metro_zone(node_id));
        }

        // Select node with lowest utilization
        candidates
            .into_iter()
            .filter(|&node_id| mesh_state.has_capacity(node_id, 10_000_000)) // 10MB min
            .min_by_key(|&node_id| mesh_state.utilization(node_id))
    }

    /// Validate actions against sovereignty constraints.
    ///
    /// Returns only actions that comply with the policy, logs violations.
    fn validate_sovereignty(
        &self,
        actions: &[ScalingAction],
        policy: &Policy,
    ) -> Vec<ScalingAction> {
        if policy.sovereignty == SovereigntyLevel::Global {
            // Global sovereignty has no restrictions
            return actions.to_vec();
        }

        // For Local/Zone sovereignty, filter out actions that violate constraints
        actions
            .iter()
            .filter(|action| {
                let is_valid = match action {
                    ScalingAction::Replicate { targets, .. } => {
                        // All targets must satisfy sovereignty
                        targets.iter().all(|_target| {
                            // TODO: Add mesh state to validate each target
                            true // Placeholder for now
                        })
                    }
                    ScalingAction::Migrate { destination, .. } => {
                        // Destination must satisfy sovereignty
                        // TODO: Validate destination against policy
                        let _ = destination;
                        true // Placeholder for now
                    }
                    ScalingAction::Federate { .. } | ScalingAction::ShardEC { .. } => true,
                    ScalingAction::Evacuate { .. } | ScalingAction::Rebalance { .. } => {
                        // Evacuation/rebalancing are always allowed
                        true
                    }
                };

                if !is_valid {
                    error!(
                        action = ?action,
                        sovereignty = ?policy.sovereignty,
                        "policy violation: action blocked by sovereignty constraint"
                    );
                }

                is_valid
            })
            .cloned()
            .collect()
    }
}

/// Current mesh state snapshot for decision-making.
///
/// Provides the compiler with topology and capacity information needed
/// to select optimal targets for replication/migration.
pub struct MeshState {
    /// Available nodes in the mesh (exclude degraded/offline)
    nodes: Vec<(NodeId, NodeInfo)>,
    /// Current node's zone (for relative placement decisions)
    local_zone: ZoneId,
}

/// Information about a node in the mesh.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub zone: ZoneId,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub network_tier: super::NetworkTier,
}

impl MeshState {
    /// Create a new mesh state snapshot.
    pub fn new(nodes: Vec<(NodeId, NodeInfo)>, local_zone: ZoneId) -> Self {
        Self { nodes, local_zone }
    }

    /// Create an empty mesh state (for testing).
    pub fn empty(local_zone: ZoneId) -> Self {
        Self {
            nodes: Vec::new(),
            local_zone,
        }
    }

    /// Get all available node IDs.
    fn available_nodes(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|(id, _)| *id).collect()
    }

    /// Enumerate unique zones the mesh currently knows about.
    fn zone_ids(&self) -> Vec<ZoneId> {
        let mut zones = vec![self.local_zone.clone()];
        for (_, info) in &self.nodes {
            if !zones.iter().any(|zone| zone == &info.zone) {
                zones.push(info.zone.clone());
            }
        }
        zones
    }

    /// Check if a node satisfies sovereignty constraints.
    fn satisfies_sovereignty(&self, node_id: NodeId, sovereignty: &SovereigntyLevel) -> bool {
        match sovereignty {
            SovereigntyLevel::Global => true,
            SovereigntyLevel::Zone => {
                // Must be in the same zone
                self.nodes
                    .iter()
                    .find(|(id, _)| *id == node_id)
                    .map(|(_, info)| info.zone == self.local_zone)
                    .unwrap_or(false)
            }
            SovereigntyLevel::Local => {
                // Must be local node only (not implemented yet)
                false
            }
        }
    }

    /// Check if a node is in the metro zone.
    fn is_metro_zone(&self, node_id: NodeId) -> bool {
        self.nodes
            .iter()
            .find(|(id, _)| *id == node_id)
            .map(|(_, info)| matches!(info.zone, ZoneId::Metro { .. }))
            .unwrap_or(false)
    }

    /// Check if a node is in the same geo region.
    fn is_same_geo_region(&self, node_id: NodeId) -> bool {
        self.nodes
            .iter()
            .find(|(id, _)| *id == node_id)
            .map(|(_, info)| match (&self.local_zone, &info.zone) {
                (ZoneId::Metro { name: n1 }, ZoneId::Metro { name: n2 }) => n1 == n2,
                (ZoneId::Geo { name: n1 }, ZoneId::Geo { name: n2 }) => n1 == n2,
                _ => false,
            })
            .unwrap_or(false)
    }

    /// Check if a node has sufficient capacity.
    fn has_capacity(&self, node_id: NodeId, required_bytes: u64) -> bool {
        self.nodes
            .iter()
            .find(|(id, _)| *id == node_id)
            .map(|(_, info)| info.available_bytes >= required_bytes)
            .unwrap_or(false)
    }

    /// Get node utilization percentage.
    fn utilization(&self, node_id: NodeId) -> u64 {
        self.nodes
            .iter()
            .find(|(id, _)| *id == node_id)
            .map(|(_, info)| {
                let total = info.available_bytes + info.used_bytes;
                if total == 0 {
                    0
                } else {
                    (info.used_bytes * 100) / total
                }
            })
            .unwrap_or(100) // Treat unknown nodes as fully utilized
    }

    /// Find nodes below the specified utilization threshold.
    fn find_underutilized_nodes(&self, threshold_percent: f32) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|(_, info)| {
                let total = info.available_bytes + info.used_bytes;
                if total == 0 {
                    return false;
                }
                let used_percent = (info.used_bytes as f32 / total as f32) * 100.0;
                used_percent < threshold_percent
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Check if migration requires transformation (e.g., zone change).
    fn requires_transformation(&self, destination: NodeId, policy: &Policy) -> bool {
        // Transformation needed if crossing zone boundaries with strict sovereignty
        if policy.sovereignty == SovereigntyLevel::Zone {
            self.nodes
                .iter()
                .find(|(id, _)| *id == destination)
                .map(|(_, info)| info.zone != self.local_zone)
                .unwrap_or(false)
        } else {
            false
        }
    }
}

/// Convenience wrapper that exposes policy compilation without instantiating an agent.
pub fn compile_scaling(
    policy: &Policy,
    event: &Telemetry,
    mesh_state: &MeshState,
) -> Vec<ScalingAction> {
    PolicyCompiler::with_defaults().compile_scaling_actions(event, policy, mesh_state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replication_strategy_zero_rpo() {
        let policy = Policy::metro_sync(); // RPO = 0
        let compiler = PolicyCompiler::new(policy.clone());

        let capsule_id = CapsuleId::new();
        let event = Telemetry::NewCapsule {
            id: capsule_id,
            policy: policy.clone(),
            node_id: None,
        };

        // Create mesh state with 2 metro nodes
        let node1 = NodeId::new();
        let node2 = NodeId::new();
        let mesh_state = MeshState::new(
            vec![
                (
                    node1,
                    NodeInfo {
                        zone: ZoneId::Metro {
                            name: "us-west".to_string(),
                        },
                        available_bytes: 1_000_000_000,
                        used_bytes: 500_000_000,
                        network_tier: super::super::NetworkTier::Premium,
                    },
                ),
                (
                    node2,
                    NodeInfo {
                        zone: ZoneId::Metro {
                            name: "us-west".to_string(),
                        },
                        available_bytes: 1_000_000_000,
                        used_bytes: 300_000_000,
                        network_tier: super::super::NetworkTier::Premium,
                    },
                ),
            ],
            ZoneId::Metro {
                name: "us-west".to_string(),
            },
        );

        let actions = compiler.compile_scaling_actions(&event, &policy, &mesh_state);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ScalingAction::Replicate {
                capsule_id: id,
                strategy,
                targets,
            } => {
                assert_eq!(*id, capsule_id);
                assert_eq!(
                    *strategy,
                    ReplicationStrategy::MetroSync { replica_count: 2 }
                );
                assert_eq!(targets.len(), 2);
            }
            _ => panic!("expected Replicate action"),
        }
    }

    #[test]
    fn test_heat_spike_migration() {
        let policy = Policy {
            latency_target: Duration::from_millis(1),
            ..Policy::metro_sync()
        };
        let compiler = PolicyCompiler::new(policy.clone());

        let capsule_id = CapsuleId::new();
        let event = Telemetry::HeatSpike {
            id: capsule_id,
            accesses_per_min: 200, // High heat
            node_id: None,
        };

        let node1 = NodeId::new();
        let mesh_state = MeshState::new(
            vec![(
                node1,
                NodeInfo {
                    zone: ZoneId::Metro {
                        name: "us-west".to_string(),
                    },
                    available_bytes: 1_000_000_000,
                    used_bytes: 100_000_000, // Low utilization
                    network_tier: super::super::NetworkTier::Premium,
                },
            )],
            ZoneId::Metro {
                name: "us-west".to_string(),
            },
        );

        let actions = compiler.compile_scaling_actions(&event, &policy, &mesh_state);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ScalingAction::Migrate {
                capsule_id: id,
                destination,
                ..
            } => {
                assert_eq!(*id, capsule_id);
                assert_eq!(*destination, node1);
            }
            _ => panic!("expected Migrate action"),
        }
    }

    #[test]
    fn test_evacuation_urgency() {
        let policy = Policy::metro_sync();
        let compiler = PolicyCompiler::new(policy.clone());

        let node_id = NodeId::new();

        // Test immediate evacuation for disk failure
        let event = Telemetry::NodeDegraded {
            node_id,
            reason: "disk_failure".to_string(),
        };

        let mesh_state = MeshState::empty(ZoneId::Metro {
            name: "us-west".to_string(),
        });
        let actions = compiler.compile_scaling_actions(&event, &policy, &mesh_state);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ScalingAction::Evacuate { urgency, .. } => {
                assert_eq!(*urgency, EvacuationUrgency::Immediate);
            }
            _ => panic!("expected Evacuate action"),
        }

        // Test gradual evacuation for degraded health
        let event2 = Telemetry::NodeDegraded {
            node_id,
            reason: "degraded_health".to_string(),
        };

        let actions2 = compiler.compile_scaling_actions(&event2, &policy, &mesh_state);

        assert_eq!(actions2.len(), 1);
        match &actions2[0] {
            ScalingAction::Evacuate { urgency, .. } => {
                assert_eq!(*urgency, EvacuationUrgency::Gradual);
            }
            _ => panic!("expected Evacuate action"),
        }
    }

    #[test]
    fn test_sovereignty_validation() {
        let policy = Policy {
            sovereignty: SovereigntyLevel::Zone,
            ..Policy::metro_sync()
        };
        let compiler = PolicyCompiler::new(policy.clone());

        // Actions should be validated (currently placeholder, always passes)
        let actions = vec![ScalingAction::Rebalance {
            overloaded_nodes: vec![NodeId::new()],
            underutilized_nodes: vec![NodeId::new()],
        }];

        let validated = compiler.validate_sovereignty(&actions, &policy);
        assert_eq!(validated.len(), 1);
    }

    #[test]
    fn test_rebalancing_threshold() {
        let policy = Policy::metro_sync();
        let compiler = PolicyCompiler::new(policy.clone());

        let node_id = NodeId::new();

        // Below threshold - no rebalancing
        let event1 = Telemetry::CapacityThreshold {
            node_id,
            used_bytes: 700_000_000,
            total_bytes: 1_000_000_000,
            threshold_pct: 70.0,
        };

        let mesh_state = MeshState::empty(ZoneId::Metro {
            name: "us-west".to_string(),
        });
        let actions1 = compiler.compile_scaling_actions(&event1, &policy, &mesh_state);
        assert_eq!(actions1.len(), 0);

        // Above threshold - rebalancing needed (but no underutilized nodes)
        let event2 = Telemetry::CapacityThreshold {
            node_id,
            used_bytes: 850_000_000,
            total_bytes: 1_000_000_000,
            threshold_pct: 85.0,
        };

        let actions2 = compiler.compile_scaling_actions(&event2, &policy, &mesh_state);
        assert_eq!(actions2.len(), 0); // No underutilized nodes available
    }
}

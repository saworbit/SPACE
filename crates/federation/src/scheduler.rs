//! Phase 9.5: The Architect (Placement Scheduler)
//!
//! Intelligent node selection based on constraints (topology, hardware)
//! and priorities (capacity, load). Replaces naive "first N nodes" approach
//! with constraint-based solver.

use crate::registry::{ClusterState, NodeMetadata, NodeStatus};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::{debug, info};

/// Describes what the volume needs from the cluster.
#[derive(Debug, Clone)]
pub struct PlacementRequirements {
    /// Size of the volume in bytes
    pub size_bytes: u64,
    /// Number of replicas required
    pub replication_factor: u32,
    /// Required tags for node selection (e.g., "region": "us-east-1")
    /// In this phase, these are optional constraints
    pub required_tags: HashMap<String, String>,
}

/// The logic engine for placing data.
///
/// This is a stateless component that takes a snapshot of ClusterState
/// and a PlacementRequirements and outputs a list of selected node IDs.
///
/// ## Architecture
///
/// The Scheduler operates in three phases:
/// 1. **Filter (Hard Rules)**: Remove nodes that cannot host the volume
///    - Node must be Active
///    - Node must have sufficient capacity
///    - Node must match required tags (future)
///
/// 2. **Weigh (Soft Rules)**: Score remaining nodes
///    - Prefer nodes with more free space
///    - Spread across racks/zones (future)
///
/// 3. **Select**: Pick top N scored nodes
///
/// ## Determinism
///
/// IMPORTANT: This scheduler is designed to run on the Leader BEFORE proposing.
/// The selected nodes are baked into the CreateVolume command, making the
/// log the single source of truth. This keeps the state machine simple and
/// deterministic across all nodes.
pub struct Scheduler;

impl Scheduler {
    /// Selects the best nodes for a new volume.
    ///
    /// # Arguments
    /// - `state`: Current cluster topology (read-only snapshot)
    /// - `req`: Volume placement requirements
    ///
    /// # Returns
    /// A sorted list of node IDs to host the volume replicas
    ///
    /// # Errors
    /// Returns an error if:
    /// - Insufficient eligible nodes are available
    /// - All nodes are down or lack capacity
    ///
    /// # Phase 9.5 Implementation
    /// Current filtering rules:
    /// - Node status must be Active
    /// - Node capacity must be >= requested size
    ///
    /// Future enhancements (Phase 10+):
    /// - Track free space (not just total capacity)
    /// - Topology awareness (rack, zone anti-affinity)
    /// - Load balancing (prefer less-loaded nodes)
    /// - Hardware constraints (SSD vs HDD, IOPS limits)
    pub fn select_nodes(state: &ClusterState, req: &PlacementRequirements) -> Result<Vec<u64>> {
        let mut candidates: Vec<&NodeMetadata> = state.nodes.values().collect();

        debug!(
            total_nodes = candidates.len(),
            required_replicas = req.replication_factor,
            size_bytes = req.size_bytes,
            "starting node selection"
        );

        // Phase 1: Hard Filters
        // =====================

        // Rule A: Node must be Active
        // Dead/Draining nodes cannot accept new volumes
        let before_status_filter = candidates.len();
        candidates.retain(|node| {
            let is_active = node.status == NodeStatus::Active;
            if !is_active {
                debug!(
                    node_id = node.id,
                    status = ?node.status,
                    "filtered out (not active)"
                );
            }
            is_active
        });
        debug!(
            filtered_count = before_status_filter - candidates.len(),
            remaining = candidates.len(),
            "applied status filter"
        );

        // Rule B: Node must have capacity
        // NOTE: This is a simplified check. In production, we need to track
        // 'available_capacity' (total - used) in NodeMetadata.
        // For now, we assume capacity is the total available space.
        let before_capacity_filter = candidates.len();
        candidates.retain(|node| {
            let has_capacity = node.capacity >= req.size_bytes;
            if !has_capacity {
                debug!(
                    node_id = node.id,
                    node_capacity = node.capacity,
                    required = req.size_bytes,
                    "filtered out (insufficient capacity)"
                );
            }
            has_capacity
        });
        debug!(
            filtered_count = before_capacity_filter - candidates.len(),
            remaining = candidates.len(),
            "applied capacity filter"
        );

        // Check if we have enough nodes
        if candidates.len() < req.replication_factor as usize {
            return Err(anyhow!(
                "Insufficient eligible nodes. Needed {}, found {}",
                req.replication_factor,
                candidates.len()
            ));
        }

        // Phase 2: Weighing / Scoring
        // ============================
        //
        // Strategy: Deterministic sort for Raft consistency
        //
        // Since this runs on the Leader BEFORE propose, we could use randomness
        // for load balancing. However, for testing and predictability in this
        // phase, we use deterministic sorting by node ID.
        //
        // Future enhancements:
        // - Score by free space (most free = higher score)
        // - Spread across racks (anti-affinity)
        // - Consider current load (IOPS, bandwidth)
        //
        // For now: Deterministic sort by ID ensures consistent test behavior
        candidates.sort_by_key(|n| n.id);

        // Phase 3: Selection
        // ==================
        let selected: Vec<u64> = candidates
            .iter()
            .take(req.replication_factor as usize)
            .map(|n| n.id)
            .collect();

        info!(
            selected_nodes = ?selected,
            volume_size_gb = req.size_bytes / (1024 * 1024 * 1024),
            replication_factor = req.replication_factor,
            "scheduler selected nodes"
        );

        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_from_multiple_active_nodes() {
        let mut state = ClusterState::default();

        // Add 3 active nodes with sufficient capacity
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 1_000_000_000, // 1 GB
                status: NodeStatus::Active,
            },
        );
        state.nodes.insert(
            2,
            NodeMetadata {
                id: 2,
                address: "10.0.0.2:8080".into(),
                capacity: 2_000_000_000, // 2 GB
                status: NodeStatus::Active,
            },
        );
        state.nodes.insert(
            3,
            NodeMetadata {
                id: 3,
                address: "10.0.0.3:8080".into(),
                capacity: 3_000_000_000, // 3 GB
                status: NodeStatus::Active,
            },
        );

        let req = PlacementRequirements {
            size_bytes: 100_000_000, // 100 MB
            replication_factor: 2,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes(&state, &req).unwrap();

        assert_eq!(result.len(), 2);
        // Deterministic sort by ID: should select nodes 1 and 2
        assert_eq!(result[0], 1);
        assert_eq!(result[1], 2);
    }

    #[test]
    fn test_filter_dead_nodes() {
        let mut state = ClusterState::default();

        // Node 1: Active, large capacity
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 1_000_000_000,
                status: NodeStatus::Active,
            },
        );

        // Node 2: Dead (should be filtered out)
        state.nodes.insert(
            2,
            NodeMetadata {
                id: 2,
                address: "10.0.0.2:8080".into(),
                capacity: 1_000_000_000,
                status: NodeStatus::Dead,
            },
        );

        // Node 3: Draining (should be filtered out)
        state.nodes.insert(
            3,
            NodeMetadata {
                id: 3,
                address: "10.0.0.3:8080".into(),
                capacity: 1_000_000_000,
                status: NodeStatus::Draining,
            },
        );

        let req = PlacementRequirements {
            size_bytes: 100_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes(&state, &req).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], 1); // Only active node selected
    }

    #[test]
    fn test_filter_insufficient_capacity() {
        let mut state = ClusterState::default();

        // Node with capacity smaller than requested size
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 50_000_000, // 50 MB (too small)
                status: NodeStatus::Active,
            },
        );

        let req = PlacementRequirements {
            size_bytes: 100_000_000, // 100 MB
            replication_factor: 1,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes(&state, &req);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Insufficient eligible nodes"));
    }

    #[test]
    fn test_insufficient_nodes_for_replication_factor() {
        let mut state = ClusterState::default();

        // Only 1 active node
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 1_000_000_000,
                status: NodeStatus::Active,
            },
        );

        // Request 3 replicas
        let req = PlacementRequirements {
            size_bytes: 100_000_000,
            replication_factor: 3,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes(&state, &req);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Needed 3, found 1"));
    }

    #[test]
    fn test_empty_cluster() {
        let state = ClusterState::default(); // No nodes

        let req = PlacementRequirements {
            size_bytes: 100_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes(&state, &req);

        assert!(result.is_err());
    }
}

//! Phase 9.5: The Architect (Placement Scheduler)
//!
//! Intelligent node selection based on constraints (topology, hardware)
//! and priorities (capacity, load). Replaces naive "first N nodes" approach
//! with constraint-based solver.

use crate::registry::{ClusterState, NodeMetadata, NodeStatus, PendingAllocations};
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

    /// Selects the best nodes for a new volume, accounting for pending allocations.
    ///
    /// This method prevents the "Smart Leader" double-spend race condition by
    /// considering in-flight proposals that haven't been committed yet.
    ///
    /// # Arguments
    /// - `state`: Current cluster topology (read-only snapshot)
    /// - `req`: Volume placement requirements
    /// - `pending`: Pending allocations from in-flight proposals
    ///
    /// # Returns
    /// A sorted list of node IDs to host the volume replicas
    ///
    /// # Errors
    /// Returns an error if insufficient eligible nodes are available after
    /// accounting for both committed and pending allocations.
    ///
    /// # Example
    /// ```no_run
    /// # use federation::scheduler::{Scheduler, PlacementRequirements};
    /// # use federation::registry::{ClusterState, PendingAllocations};
    /// # use std::collections::HashMap;
    /// let state = ClusterState::default();
    /// let pending = PendingAllocations::new();
    /// let req = PlacementRequirements {
    ///     size_bytes: 1_000_000_000,
    ///     replication_factor: 3,
    ///     required_tags: HashMap::new(),
    /// };
    /// let nodes = Scheduler::select_nodes_with_pending(&state, &req, &pending);
    /// ```
    pub fn select_nodes_with_pending(
        state: &ClusterState,
        req: &PlacementRequirements,
        pending: &PendingAllocations,
    ) -> Result<Vec<u64>> {
        let mut candidates: Vec<&NodeMetadata> = state.nodes.values().collect();

        debug!(
            total_nodes = candidates.len(),
            required_replicas = req.replication_factor,
            size_bytes = req.size_bytes,
            "starting node selection with pending allocations"
        );

        // Phase 1: Hard Filters
        // =====================

        // Rule A: Node must be Active
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

        // Rule B: Node must have capacity (accounting for pending allocations)
        // This is the key difference from select_nodes() - we subtract pending
        // allocations from available capacity to prevent over-provisioning.
        let before_capacity_filter = candidates.len();
        candidates.retain(|node| {
            let pending_size = pending.pending_size_for_node(node.id);
            let available_capacity = node.capacity.saturating_sub(pending_size);
            let has_capacity = available_capacity >= req.size_bytes;

            if !has_capacity {
                debug!(
                    node_id = node.id,
                    total_capacity = node.capacity,
                    pending_size = pending_size,
                    available_capacity = available_capacity,
                    required = req.size_bytes,
                    "filtered out (insufficient capacity after pending)"
                );
            }
            has_capacity
        });
        debug!(
            filtered_count = before_capacity_filter - candidates.len(),
            remaining = candidates.len(),
            "applied capacity filter (with pending)"
        );

        // Check if we have enough nodes
        if candidates.len() < req.replication_factor as usize {
            return Err(anyhow!(
                "Insufficient eligible nodes after accounting for pending allocations. \
                 Needed {}, found {}",
                req.replication_factor,
                candidates.len()
            ));
        }

        // Phase 2: Weighing / Scoring
        // Sort by available capacity (descending), then by ID for determinism
        candidates.sort_by(|a, b| {
            let a_available = a
                .capacity
                .saturating_sub(pending.pending_size_for_node(a.id));
            let b_available = b
                .capacity
                .saturating_sub(pending.pending_size_for_node(b.id));
            // Primary: more available capacity first
            // Secondary: lower ID for determinism
            b_available.cmp(&a_available).then(a.id.cmp(&b.id))
        });

        // Phase 3: Selection
        let selected: Vec<u64> = candidates
            .iter()
            .take(req.replication_factor as usize)
            .map(|n| n.id)
            .collect();

        info!(
            selected_nodes = ?selected,
            volume_size_gb = req.size_bytes / (1024 * 1024 * 1024),
            replication_factor = req.replication_factor,
            "scheduler selected nodes (with pending awareness)"
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

    #[test]
    fn test_pending_allocations_reduce_available_capacity() {
        let mut state = ClusterState::default();
        let pending = PendingAllocations::new();

        // Node with 1 GB capacity
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 1_000_000_000, // 1 GB
                status: NodeStatus::Active,
            },
        );

        // First request: 500 MB - should succeed
        let req1 = PlacementRequirements {
            size_bytes: 500_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };
        let result1 = Scheduler::select_nodes_with_pending(&state, &req1, &pending).unwrap();
        assert_eq!(result1, vec![1]);

        // Register pending allocation for first request
        pending.register("vol-1".to_string(), 500_000_000, vec![1]);

        // Second request: 600 MB - should fail (only 500 MB available after pending)
        let req2 = PlacementRequirements {
            size_bytes: 600_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };
        let result2 = Scheduler::select_nodes_with_pending(&state, &req2, &pending);
        assert!(result2.is_err());
        assert!(result2
            .unwrap_err()
            .to_string()
            .contains("Insufficient eligible nodes"));

        // Third request: 400 MB - should succeed (500 MB available)
        let req3 = PlacementRequirements {
            size_bytes: 400_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };
        let result3 = Scheduler::select_nodes_with_pending(&state, &req3, &pending).unwrap();
        assert_eq!(result3, vec![1]);
    }

    #[test]
    fn test_pending_allocations_prefer_nodes_with_more_available() {
        let mut state = ClusterState::default();
        let pending = PendingAllocations::new();

        // Node 1: 2 GB capacity
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 2_000_000_000,
                status: NodeStatus::Active,
            },
        );

        // Node 2: 2 GB capacity
        state.nodes.insert(
            2,
            NodeMetadata {
                id: 2,
                address: "10.0.0.2:8080".into(),
                capacity: 2_000_000_000,
                status: NodeStatus::Active,
            },
        );

        // Add pending allocation on node 1 (1.5 GB pending)
        pending.register("vol-existing".to_string(), 1_500_000_000, vec![1]);

        // Request 1 replica - should prefer node 2 (more available capacity)
        let req = PlacementRequirements {
            size_bytes: 100_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };

        let result = Scheduler::select_nodes_with_pending(&state, &req, &pending).unwrap();
        // Node 2 has 2 GB available, Node 1 has 0.5 GB available
        // Should prefer node 2 (more available)
        assert_eq!(result, vec![2]);
    }

    #[test]
    fn test_pending_allocations_released_after_commit() {
        let pending = PendingAllocations::new();

        // Register allocation
        pending.register("vol-1".to_string(), 500_000_000, vec![1, 2]);

        // Verify it's tracked
        assert!(pending.has_pending("vol-1"));
        assert_eq!(pending.pending_size_for_node(1), 500_000_000);
        assert_eq!(pending.pending_size_for_node(2), 500_000_000);

        // Release allocation
        pending.release("vol-1");

        // Verify it's no longer tracked
        assert!(!pending.has_pending("vol-1"));
        assert_eq!(pending.pending_size_for_node(1), 0);
        assert_eq!(pending.pending_size_for_node(2), 0);
    }

    #[test]
    fn test_concurrent_allocations_prevent_double_spend() {
        let mut state = ClusterState::default();
        let pending = PendingAllocations::new();

        // Single node with 1 GB capacity
        state.nodes.insert(
            1,
            NodeMetadata {
                id: 1,
                address: "10.0.0.1:8080".into(),
                capacity: 1_000_000_000, // 1 GB
                status: NodeStatus::Active,
            },
        );

        // Simulate two concurrent requests, each wanting 600 MB
        let req = PlacementRequirements {
            size_bytes: 600_000_000,
            replication_factor: 1,
            required_tags: HashMap::new(),
        };

        // Request A: succeeds, selects node 1
        let result_a = Scheduler::select_nodes_with_pending(&state, &req, &pending).unwrap();
        assert_eq!(result_a, vec![1]);

        // Request A registers pending allocation
        pending.register("vol-a".to_string(), 600_000_000, vec![1]);

        // Request B: fails because node 1 only has 400 MB available after pending
        let result_b = Scheduler::select_nodes_with_pending(&state, &req, &pending);
        assert!(result_b.is_err());

        // After Request A commits, release pending
        pending.release("vol-a");

        // Now Request B (retry) can succeed
        let result_b_retry = Scheduler::select_nodes_with_pending(&state, &req, &pending).unwrap();
        assert_eq!(result_b_retry, vec![1]);
    }
}

//! Integration tests for Phase 9.5 Placement Scheduler
//!
//! These tests verify that the scheduler correctly filters nodes based on
//! hard constraints and selects optimal placement for volumes.

use federation::{ClusterState, NodeMetadata, NodeStatus, PlacementRequirements, Scheduler};
use std::collections::HashMap;

#[test]
fn test_scheduler_filters_dead_nodes() {
    let mut state = ClusterState::default();

    // Add Node 1 (Active, Big)
    state.nodes.insert(
        1,
        NodeMetadata {
            id: 1,
            address: "10.0.0.1:8080".into(),
            capacity: 1_000_000_000,
            status: NodeStatus::Active,
        },
    );

    // Add Node 2 (Dead)
    state.nodes.insert(
        2,
        NodeMetadata {
            id: 2,
            address: "10.0.0.2:8080".into(),
            capacity: 1_000_000_000,
            status: NodeStatus::Dead,
        },
    );

    let req = PlacementRequirements {
        size_bytes: 100_000_000,
        replication_factor: 1,
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0], 1); // Should be Node 1
}

#[test]
fn test_scheduler_filters_draining_nodes() {
    let mut state = ClusterState::default();

    // Add Node 1 (Active)
    state.nodes.insert(
        1,
        NodeMetadata {
            id: 1,
            address: "10.0.0.1:8080".into(),
            capacity: 1_000_000_000,
            status: NodeStatus::Active,
        },
    );

    // Add Node 2 (Draining - should be filtered out)
    state.nodes.insert(
        2,
        NodeMetadata {
            id: 2,
            address: "10.0.0.2:8080".into(),
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
    assert_eq!(result[0], 1);
}

#[test]
fn test_scheduler_insufficient_capacity() {
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
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Insufficient eligible nodes"));
    assert!(err_msg.contains("Needed 1, found 0"));
}

#[test]
fn test_scheduler_insufficient_nodes_for_replication() {
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

    // Request 3 replicas but only 1 node available
    let req = PlacementRequirements {
        size_bytes: 100_000_000,
        replication_factor: 3,
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req);

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Needed 3, found 1"));
}

#[test]
fn test_scheduler_empty_cluster() {
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
fn test_scheduler_deterministic_selection() {
    let mut state = ClusterState::default();

    // Add 5 active nodes with varying capacities
    for i in 1..=5 {
        state.nodes.insert(
            i,
            NodeMetadata {
                id: i,
                address: format!("10.0.0.{}:8080", i),
                capacity: i * 1_000_000_000, // 1-5 GB
                status: NodeStatus::Active,
            },
        );
    }

    let req = PlacementRequirements {
        size_bytes: 100_000_000,
        replication_factor: 3,
        required_tags: HashMap::new(),
    };

    // Run scheduler multiple times - should get same result (deterministic)
    let result1 = Scheduler::select_nodes(&state, &req).unwrap();
    let result2 = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result1, result2);
    assert_eq!(result1.len(), 3);

    // Should select nodes 1, 2, 3 (deterministic sort by ID)
    assert_eq!(result1[0], 1);
    assert_eq!(result1[1], 2);
    assert_eq!(result1[2], 3);
}

#[test]
fn test_scheduler_mixed_cluster() {
    let mut state = ClusterState::default();

    // Node 1: Active, sufficient capacity
    state.nodes.insert(
        1,
        NodeMetadata {
            id: 1,
            address: "10.0.0.1:8080".into(),
            capacity: 2_000_000_000,
            status: NodeStatus::Active,
        },
    );

    // Node 2: Dead (filtered out)
    state.nodes.insert(
        2,
        NodeMetadata {
            id: 2,
            address: "10.0.0.2:8080".into(),
            capacity: 2_000_000_000,
            status: NodeStatus::Dead,
        },
    );

    // Node 3: Active but insufficient capacity (filtered out)
    state.nodes.insert(
        3,
        NodeMetadata {
            id: 3,
            address: "10.0.0.3:8080".into(),
            capacity: 50_000_000,
            status: NodeStatus::Active,
        },
    );

    // Node 4: Active, sufficient capacity
    state.nodes.insert(
        4,
        NodeMetadata {
            id: 4,
            address: "10.0.0.4:8080".into(),
            capacity: 2_000_000_000,
            status: NodeStatus::Active,
        },
    );

    // Node 5: Draining (filtered out)
    state.nodes.insert(
        5,
        NodeMetadata {
            id: 5,
            address: "10.0.0.5:8080".into(),
            capacity: 2_000_000_000,
            status: NodeStatus::Draining,
        },
    );

    let req = PlacementRequirements {
        size_bytes: 100_000_000,
        replication_factor: 2,
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result.len(), 2);
    // Should select nodes 1 and 4 (the only eligible nodes)
    assert_eq!(result[0], 1);
    assert_eq!(result[1], 4);
}

#[test]
fn test_scheduler_exact_capacity_match() {
    let mut state = ClusterState::default();

    // Node with capacity exactly matching the request
    state.nodes.insert(
        1,
        NodeMetadata {
            id: 1,
            address: "10.0.0.1:8080".into(),
            capacity: 100_000_000, // Exactly 100 MB
            status: NodeStatus::Active,
        },
    );

    let req = PlacementRequirements {
        size_bytes: 100_000_000, // Exactly 100 MB
        replication_factor: 1,
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0], 1);
}

#[test]
fn test_scheduler_large_cluster() {
    let mut state = ClusterState::default();

    // Simulate a large cluster with 100 nodes
    for i in 1..=100 {
        state.nodes.insert(
            i,
            NodeMetadata {
                id: i,
                address: format!("10.0.{}.{}:8080", i / 256, i % 256),
                capacity: 10_000_000_000, // 10 GB each
                status: NodeStatus::Active,
            },
        );
    }

    let req = PlacementRequirements {
        size_bytes: 1_000_000_000, // 1 GB
        replication_factor: 5,
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result.len(), 5);

    // Should select first 5 nodes (deterministic)
    for (i, &node_id) in result.iter().enumerate().take(5) {
        assert_eq!(node_id, (i + 1) as u64);
    }

    // Verify all selected nodes are unique
    let unique_nodes: std::collections::HashSet<_> = result.iter().collect();
    assert_eq!(unique_nodes.len(), 5);
}

#[test]
fn test_scheduler_all_nodes_eligible() {
    let mut state = ClusterState::default();

    // Add 10 nodes, all eligible
    for i in 1..=10 {
        state.nodes.insert(
            i,
            NodeMetadata {
                id: i,
                address: format!("10.0.0.{}:8080", i),
                capacity: 5_000_000_000,
                status: NodeStatus::Active,
            },
        );
    }

    let req = PlacementRequirements {
        size_bytes: 1_000_000_000,
        replication_factor: 10, // Use all nodes
        required_tags: HashMap::new(),
    };

    let result = Scheduler::select_nodes(&state, &req).unwrap();

    assert_eq!(result.len(), 10);

    // Verify all nodes are selected
    for i in 1..=10 {
        assert!(result.contains(&i));
    }
}

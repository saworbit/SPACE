//! Phase 9.3: Registry state machine tests

use federation::registry::{
    build_create_volume_cmd, build_delete_volume_cmd, build_move_replica_cmd,
    build_register_node_cmd, NodeStatus, Registry,
};

#[test]
fn test_registry_transitions() {
    let registry = Registry::new();

    // Initial state
    assert!(registry.get_state().nodes.is_empty());
    assert!(registry.get_state().volumes.is_empty());
    assert_eq!(registry.get_state().last_applied_index, 0);

    // Register node
    let cmd1 = build_register_node_cmd(1, "127.0.0.1:4422", 1024 * 1024 * 1024);
    registry.apply(1, &cmd1).unwrap();

    // Create volume (Phase 9.5: include pre-selected replicas)
    let cmd2 = build_create_volume_cmd("vol-test-1", 1024 * 1024 * 100, 3, vec![1]);
    registry.apply(2, &cmd2).unwrap();

    // Assert state
    let state = registry.get_state();
    assert!(state.nodes.contains_key(&1));
    assert_eq!(state.nodes.get(&1).unwrap().status, NodeStatus::Active);
    assert_eq!(state.nodes.get(&1).unwrap().address, "127.0.0.1:4422");
    assert!(state.volumes.contains_key("vol-test-1"));
    assert_eq!(
        state.volumes.get("vol-test-1").unwrap().size,
        1024 * 1024 * 100
    );
    assert_eq!(state.last_applied_index, 2);
}

#[test]
fn test_registry_idempotency() {
    let registry = Registry::new();

    let cmd = build_register_node_cmd(1, "127.0.0.1:4422", 1000);

    // Apply twice with same index
    registry.apply(1, &cmd).unwrap();
    registry.apply(1, &cmd).unwrap(); // Should be no-op

    let state = registry.get_state();
    assert_eq!(state.nodes.len(), 1);
    assert_eq!(state.last_applied_index, 1);

    // Apply with lower index (should be ignored)
    let cmd2 = build_register_node_cmd(2, "127.0.0.1:4423", 2000);
    registry.apply(0, &cmd2).unwrap(); // Lower index than last_applied

    let state = registry.get_state();
    assert_eq!(state.nodes.len(), 1); // Node 2 was not added
    assert!(!state.nodes.contains_key(&2));
}

#[test]
fn test_registry_snapshot_restore() {
    let registry1 = Registry::new();

    // Build state
    let cmd1 = build_register_node_cmd(1, "addr1", 1000);
    let cmd2 = build_register_node_cmd(2, "addr2", 2000);
    let cmd3 = build_create_volume_cmd("vol-1", 500, 2, vec![1, 2]);

    registry1.apply(1, &cmd1).unwrap();
    registry1.apply(2, &cmd2).unwrap();
    registry1.apply(3, &cmd3).unwrap();

    // Take snapshot
    let snapshot = registry1.take_snapshot().unwrap();

    // Restore to new registry
    let registry2 = Registry::new();
    registry2.restore_snapshot(&snapshot).unwrap();

    // Verify
    let state = registry2.get_state();
    assert_eq!(state.nodes.len(), 2);
    assert_eq!(state.volumes.len(), 1);
    assert_eq!(state.last_applied_index, 3);
    assert!(state.volumes.contains_key("vol-1"));
    assert!(state.nodes.contains_key(&1));
    assert!(state.nodes.contains_key(&2));
}

#[test]
fn test_registry_delete_volume() {
    let registry = Registry::new();

    // Register nodes
    let cmd1 = build_register_node_cmd(1, "node1", 1000);
    let cmd2 = build_register_node_cmd(2, "node2", 2000);
    registry.apply(1, &cmd1).unwrap();
    registry.apply(2, &cmd2).unwrap();

    // Create volume
    let cmd3 = build_create_volume_cmd("vol-to-delete", 500, 2, vec![1, 2]);
    registry.apply(3, &cmd3).unwrap();

    // Verify volume exists
    let state = registry.get_state();
    assert!(state.volumes.contains_key("vol-to-delete"));

    // Delete volume
    let cmd4 = build_delete_volume_cmd("vol-to-delete");
    registry.apply(4, &cmd4).unwrap();

    // Verify volume is deleted
    let state = registry.get_state();
    assert!(!state.volumes.contains_key("vol-to-delete"));
    assert_eq!(state.last_applied_index, 4);
}

#[test]
fn test_registry_move_replica() {
    let registry = Registry::new();

    // Register 3 nodes
    registry
        .apply(1, &build_register_node_cmd(1, "node1", 1000))
        .unwrap();
    registry
        .apply(2, &build_register_node_cmd(2, "node2", 2000))
        .unwrap();
    registry
        .apply(3, &build_register_node_cmd(3, "node3", 3000))
        .unwrap();

    // Create volume with 2 replicas
    registry
        .apply(4, &build_create_volume_cmd("vol-move", 500, 2, vec![1, 2]))
        .unwrap();

    // Verify initial replica placement and get the actual replicas
    let state = registry.get_state();
    let vol = state.volumes.get("vol-move").unwrap();
    assert_eq!(vol.replicas.len(), 2);
    let from_node = vol.replicas[0]; // Pick first replica
                                     // Find a node that is NOT in the current replicas
    let to_node = [1, 2, 3]
        .iter()
        .find(|&n| !vol.replicas.contains(n))
        .copied()
        .unwrap();

    // Move replica from one node to another
    let cmd = build_move_replica_cmd("vol-move", from_node, to_node);
    registry.apply(5, &cmd).unwrap();

    // Verify replica was moved
    let state = registry.get_state();
    let vol = state.volumes.get("vol-move").unwrap();
    assert_eq!(vol.replicas.len(), 2);
    assert!(!vol.replicas.contains(&from_node));
    assert!(vol.replicas.contains(&to_node));
}

#[test]
fn test_registry_volume_placement() {
    let registry = Registry::new();

    // Register only 2 nodes
    registry
        .apply(1, &build_register_node_cmd(1, "node1", 1000))
        .unwrap();
    registry
        .apply(2, &build_register_node_cmd(2, "node2", 2000))
        .unwrap();

    // Try to create volume with replication factor 3 (but only provide 2 replicas)
    registry
        .apply(
            3,
            &build_create_volume_cmd("vol-under-replicated", 500, 3, vec![1, 2]),
        )
        .unwrap();

    // Volume should be created with only 2 replicas (warning logged)
    let state = registry.get_state();
    let vol = state.volumes.get("vol-under-replicated").unwrap();
    assert_eq!(vol.replicas.len(), 2); // Only 2 nodes available
    assert!(vol.replicas.contains(&1));
    assert!(vol.replicas.contains(&2));
}

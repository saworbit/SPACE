//! Integration tests for the Reconciler
//!
//! These tests verify the complete "Self-Driving" control loop:
//! 1. Registry state changes (via Raft apply)
//! 2. Reconciler detects the change
//! 3. Foundry converges to match desired state

use std::sync::Arc;
use std::time::Duration;

use capsule_registry::CapsuleRegistry;
use capsule_registry::pipeline::WritePipeline;
use foundry::backend::VolumeId;
use foundry::snapshot::SnapshotEngine;
use foundry::Foundry;
use nvram_sim::NvramLog;
use tempfile::TempDir;
use tokio::time::sleep;

use federation::{build_create_volume_cmd, build_register_node_cmd, Registry};
use podms_orchestrator::Reconciler;

/// Test that the reconciler creates volumes when they appear in the registry.
///
/// This verifies the CREATE path:
/// 1. Volume is added to Registry via Raft
/// 2. Reconciler detects it's missing from Foundry
/// 3. Reconciler creates the volume in Foundry
#[tokio::test]
async fn test_reconciliation_creates_volume() {
    // 1. Setup Foundry with temp directory
    let temp_dir = TempDir::new().unwrap();
    let foundry = Arc::new(Foundry::with_data_dir(temp_dir.path()));

    // 2. Setup Registry
    let registry = Arc::new(Registry::new());

    // 3. Register node 1 (required for placement logic)
    // The placement strategy uses "first N available nodes", so we need
    // to register the node before creating volumes.
    let node_id = 1u64;
    let register_cmd = build_register_node_cmd(node_id, "127.0.0.1:4422", 1_000_000_000);
    registry.apply(1, &register_cmd).unwrap();

    // 4. Create a volume with UUID-format ID
    // IMPORTANT: The reconciler requires volume IDs to be valid UUIDs.
    // The Registry stores them as Strings, but they must parse to VolumeId.
    let vol_id_uuid = VolumeId::new();
    let vol_id_str = vol_id_uuid.to_string();
    let size_bytes = 10 * 1024 * 1024; // 10 MB

    let create_cmd = build_create_volume_cmd(&vol_id_str, size_bytes, 1, vec![1]);
    registry.apply(2, &create_cmd).unwrap();

    // 5. Verify volume was assigned to node 1
    // The placement logic should have assigned it to our test node.
    let state = registry.get_state();
    assert!(
        state.volumes.contains_key(&vol_id_str),
        "Volume not found in registry after apply"
    );
    assert!(
        state.volumes[&vol_id_str].replicas.contains(&node_id),
        "Volume not assigned to node {}",
        node_id
    );

    // 6. Create reconciler with SnapshotEngine
    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));
    let reconciler = Reconciler::new(node_id, foundry.clone(), registry.clone(), snapshot_engine);

    // 7. Spawn reconciler in background
    // The reconciler runs indefinitely, so we spawn it as a background task.
    tokio::spawn(async move {
        reconciler.run().await;
    });

    // 8. Wait for reconciliation loop to run
    // The default interval is 5 seconds, so we wait 6 seconds to ensure
    // at least one reconciliation cycle completes.
    sleep(Duration::from_secs(6)).await;

    // 9. Verify volume was created in Foundry
    let volumes = foundry.list_volumes().await;
    assert!(
        volumes.contains(&vol_id_uuid),
        "Volume {} was not created by reconciler. Found: {:?}",
        vol_id_uuid,
        volumes
    );

    // Cleanup: temp_dir is automatically deleted when it goes out of scope
}

/// Test that the reconciler deletes zombie volumes.
///
/// This verifies the DELETE path:
/// 1. Volume exists in Foundry but not in Registry (for this node)
/// 2. Reconciler detects the zombie
/// 3. Reconciler deletes the volume from Foundry
///
/// A "zombie volume" can occur when:
/// - Volume was moved to another node
/// - Volume was deleted from the registry
/// - Node was removed from the replica set
#[tokio::test]
async fn test_reconciliation_deletes_zombie_volume() {
    // 1. Setup
    let temp_dir = TempDir::new().unwrap();
    let foundry = Arc::new(Foundry::with_data_dir(temp_dir.path()));
    let registry = Arc::new(Registry::new());

    // 2. Register node
    // Even though we won't assign any volumes to this node in the registry,
    // we still need to register it for the test setup.
    let node_id = 1u64;
    let register_cmd = build_register_node_cmd(node_id, "127.0.0.1:4422", 1_000_000_000);
    registry.apply(1, &register_cmd).unwrap();

    // 3. Create a "zombie" volume directly in Foundry (not in registry)
    // This simulates a volume that was left behind after a migration or
    // manual deletion from the registry.
    let zombie_id = VolumeId::new();
    foundry
        .create_volume(zombie_id, 10 * 1024 * 1024, None)
        .await
        .unwrap();

    // 4. Verify zombie exists in Foundry
    let volumes_before = foundry.list_volumes().await;
    assert!(
        volumes_before.contains(&zombie_id),
        "Zombie volume should exist in Foundry before reconciliation"
    );

    // 5. Verify zombie does NOT exist in registry for this node
    let state = registry.get_state();
    let zombie_str = zombie_id.to_string();
    assert!(
        !state.volumes.contains_key(&zombie_str)
            || !state.volumes[&zombie_str].replicas.contains(&node_id),
        "Zombie volume should not be assigned to this node in registry"
    );

    // 6. Start reconciler with SnapshotEngine
    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));
    let reconciler = Reconciler::new(node_id, foundry.clone(), registry.clone(), snapshot_engine);
    tokio::spawn(async move {
        reconciler.run().await;
    });

    // 7. Wait for reconciliation
    sleep(Duration::from_secs(6)).await;

    // 8. Verify zombie was deleted
    let volumes_after = foundry.list_volumes().await;
    assert!(
        !volumes_after.contains(&zombie_id),
        "Zombie volume {} was not deleted. Found: {:?}",
        zombie_id,
        volumes_after
    );

    // Cleanup: temp_dir is automatically deleted when it goes out of scope
}

/// Test that the reconciler handles multiple volumes correctly.
///
/// This is a more comprehensive test that verifies:
/// 1. Multiple volumes can be created
/// 2. Reconciler only acts on volumes assigned to this specific node
#[tokio::test]
async fn test_reconciliation_with_multiple_volumes() {
    // Setup
    let temp_dir = TempDir::new().unwrap();
    let foundry = Arc::new(Foundry::with_data_dir(temp_dir.path()));
    let registry = Arc::new(Registry::new());

    // Register node 1 only (to ensure predictable placement)
    let node_id_1 = 1u64;
    let register_cmd_1 = build_register_node_cmd(node_id_1, "127.0.0.1:4422", 1_000_000_000);
    registry.apply(1, &register_cmd_1).unwrap();

    // Create two volumes - both will be assigned to node 1 (only node available)
    let vol_1_id = VolumeId::new();
    let vol_1_str = vol_1_id.to_string();
    let create_cmd_1 = build_create_volume_cmd(&vol_1_str, 10 * 1024 * 1024, 1, vec![1]);
    registry.apply(2, &create_cmd_1).unwrap();

    let vol_2_id = VolumeId::new();
    let vol_2_str = vol_2_id.to_string();
    let create_cmd_2 = build_create_volume_cmd(&vol_2_str, 20 * 1024 * 1024, 1, vec![1]);
    registry.apply(3, &create_cmd_2).unwrap();

    // Verify both volumes are assigned to node 1
    let state = registry.get_state();
    assert!(
        state.volumes[&vol_1_str].replicas.contains(&node_id_1),
        "Volume 1 should be assigned to node 1"
    );
    assert!(
        state.volumes[&vol_2_str].replicas.contains(&node_id_1),
        "Volume 2 should be assigned to node 1"
    );

    // Start reconciler for node 1 with SnapshotEngine
    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));
    let reconciler = Reconciler::new(
        node_id_1,
        foundry.clone(),
        registry.clone(),
        snapshot_engine,
    );
    tokio::spawn(async move {
        reconciler.run().await;
    });

    // Wait for reconciliation
    sleep(Duration::from_secs(6)).await;

    // Verify both volumes were created
    let volumes = foundry.list_volumes().await;
    assert!(volumes.contains(&vol_1_id), "Volume 1 not created");
    assert!(volumes.contains(&vol_2_id), "Volume 2 not created");
    assert_eq!(
        volumes.len(),
        2,
        "Should have exactly 2 volumes, found: {:?}",
        volumes
    );
}

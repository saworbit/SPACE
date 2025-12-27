//! Integration test for Volume Hydration (Phase 9.6)
//!
//! This test validates the complete flow:
//! 1. Create an origin volume and write data to it
//! 2. Take a snapshot of the volume
//! 3. Command the Registry to create a new volume from the snapshot
//! 4. Reconciler detects the new volume and hydrates it
//! 5. Verify the restored volume contains the original data

use std::sync::Arc;

use bytes::Bytes;
use capsule_registry::pipeline::WritePipeline;
use capsule_registry::CapsuleRegistry;
use common::Policy;
use foundry::backend::VolumeId;
use foundry::snapshot::SnapshotEngine;
use foundry::Foundry;
use nvram_sim::NvramLog;

use federation::{build_create_volume_cmd_with_source, Registry};
use podms_orchestrator::Reconciler;

#[tokio::test]
async fn test_volume_hydration_flow() {
    // 1. Setup Components
    let temp_dir = tempfile::tempdir().unwrap();
    let foundry = Arc::new(Foundry::new());
    let registry = Arc::new(Registry::new());

    // Setup WritePipeline with proper dependencies
    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

    let node_id = 1;
    let reconciler = Reconciler::new(
        node_id,
        foundry.clone(),
        registry.clone(),
        snapshot_engine.clone(),
    );

    // 2. Create Origin Volume & Write Data
    let origin_id = VolumeId::new();
    foundry
        .create_volume(origin_id, 1024 * 1024, None)
        .await
        .unwrap();

    let origin_vol = foundry.get_volume(origin_id).await.unwrap();
    let test_data = Bytes::from("Important Data - Time Travel Test");
    origin_vol.write_at(0, test_data.clone()).await.unwrap();
    origin_vol.sync().await.unwrap();

    // 3. Take Snapshot
    let capsule_id = snapshot_engine
        .take_snapshot(origin_id, origin_vol.clone(), Policy::default())
        .await
        .unwrap();

    println!(
        "Created snapshot: {} from origin volume: {}",
        capsule_id.as_uuid(),
        origin_id
    );

    // 4. Inject "Create Volume FROM Snapshot" command into Registry
    let restored_vol_id = "restored-vol-1";
    let cmd = build_create_volume_cmd_with_source(
        restored_vol_id,
        1024 * 1024,
        1,
        vec![node_id], // Assign to our node
        Some(capsule_id.as_uuid().to_string()),
    );

    registry.apply(1, &cmd).unwrap();

    // 5. Run Reconciler Step
    reconciler.reconcile_step().await.unwrap();

    // 6. Verify New Volume Contains Data
    let restored_id: VolumeId = restored_vol_id.parse().unwrap();
    let restored_vol = foundry.get_volume(restored_id).await.unwrap();

    let read_data = restored_vol.read_at(0, test_data.len()).await.unwrap();
    assert_eq!(
        read_data, test_data,
        "Restored volume should contain original data"
    );

    println!("✓ Hydration test passed: Data successfully restored from snapshot");
}

#[tokio::test]
async fn test_hydration_failure_cleanup() {
    // Test that if hydration fails, the partial volume is cleaned up

    let temp_dir = tempfile::tempdir().unwrap();
    let foundry = Arc::new(Foundry::new());
    let registry = Arc::new(Registry::new());

    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

    let node_id = 1;
    let reconciler = Reconciler::new(
        node_id,
        foundry.clone(),
        registry.clone(),
        snapshot_engine.clone(),
    );

    // Create a command with an INVALID capsule ID (doesn't exist)
    let restored_vol_id = "restored-vol-2";
    let fake_capsule_id = "00000000-0000-0000-0000-000000000000";

    let cmd = build_create_volume_cmd_with_source(
        restored_vol_id,
        1024 * 1024,
        1,
        vec![node_id],
        Some(fake_capsule_id.to_string()),
    );

    registry.apply(1, &cmd).unwrap();

    // Reconciler should fail and clean up
    let result = reconciler.reconcile_step().await;
    assert!(
        result.is_err(),
        "Reconciliation should fail with invalid capsule"
    );

    // Verify volume was cleaned up
    let restored_id: VolumeId = restored_vol_id.parse().unwrap();
    let volumes = foundry.list_volumes().await;
    assert!(
        !volumes.contains(&restored_id),
        "Failed volume should be cleaned up"
    );

    println!("✓ Cleanup test passed: Failed hydration cleaned up partial volume");
}

#[tokio::test]
async fn test_hydration_with_larger_snapshot() {
    // Test hydrating a volume from a larger snapshot
    // This validates the resize logic in restore_snapshot

    let temp_dir = tempfile::tempdir().unwrap();
    let foundry = Arc::new(Foundry::new());
    let registry = Arc::new(Registry::new());

    let capsule_registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(temp_dir.path().join("nvram.log")).unwrap();
    let pipeline = Arc::new(WritePipeline::new(capsule_registry, nvram));
    let snapshot_engine = Arc::new(SnapshotEngine::new(pipeline));

    let node_id = 1;
    let reconciler = Reconciler::new(
        node_id,
        foundry.clone(),
        registry.clone(),
        snapshot_engine.clone(),
    );

    // Create a larger origin volume (2MB) with data at different offsets
    let origin_id = VolumeId::new();
    let origin_size = 2 * 1024 * 1024;
    foundry
        .create_volume(origin_id, origin_size, None)
        .await
        .unwrap();

    let origin_vol = foundry.get_volume(origin_id).await.unwrap();

    // Write data at beginning
    let data_start = Bytes::from("START");
    origin_vol.write_at(0, data_start.clone()).await.unwrap();

    // Write data at middle
    let data_middle = Bytes::from("MIDDLE");
    origin_vol
        .write_at(1024 * 1024, data_middle.clone())
        .await
        .unwrap();

    // Write data near end
    let data_end = Bytes::from("END");
    origin_vol
        .write_at(origin_size - data_end.len() as u64, data_end.clone())
        .await
        .unwrap();

    origin_vol.sync().await.unwrap();

    // Take snapshot
    let capsule_id = snapshot_engine
        .take_snapshot(origin_id, origin_vol.clone(), Policy::default())
        .await
        .unwrap();

    // Create smaller volume and hydrate (should be resized)
    let restored_vol_id = "restored-vol-3";
    let cmd = build_create_volume_cmd_with_source(
        restored_vol_id,
        1024 * 1024, // Start with smaller size
        1,
        vec![node_id],
        Some(capsule_id.as_uuid().to_string()),
    );

    registry.apply(1, &cmd).unwrap();
    reconciler.reconcile_step().await.unwrap();

    // Verify data at all positions
    let restored_id: VolumeId = restored_vol_id.parse().unwrap();
    let restored_vol = foundry.get_volume(restored_id).await.unwrap();

    let read_start = restored_vol.read_at(0, data_start.len()).await.unwrap();
    assert_eq!(read_start, data_start);

    let read_middle = restored_vol
        .read_at(1024 * 1024, data_middle.len())
        .await
        .unwrap();
    assert_eq!(read_middle, data_middle);

    let read_end = restored_vol
        .read_at(origin_size - data_end.len() as u64, data_end.len())
        .await
        .unwrap();
    assert_eq!(read_end, data_end);

    // Verify size was adjusted
    assert_eq!(
        restored_vol.size().await.unwrap(),
        origin_size,
        "Volume should be resized to match snapshot"
    );

    println!("✓ Large snapshot test passed: Volume resized and data restored at all offsets");
}

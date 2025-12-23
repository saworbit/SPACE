//! Integration tests for the Foundry Snapshot Engine.
//!
//! This test suite verifies that data survives the round-trip:
//! Volume -> Snapshot -> New Volume

use bytes::Bytes;
use capsule_registry::pipeline::WritePipeline;
use capsule_registry::CapsuleRegistry;
use common::Policy;
use foundry::snapshot::SnapshotEngine;
use foundry::{BackendType, Foundry, VolumeId};
use nvram_sim::NvramLog;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_snapshot_roundtrip() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Setup Infrastructure
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    // Setup minimal Registry/Pipeline
    let registry_path = temp_dir.path().join("registry.db");
    let nvram_path = temp_dir.path().join("nvram.log");

    let registry = CapsuleRegistry::open(registry_path.to_str().unwrap()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_str().unwrap()).unwrap();
    let pipeline = Arc::new(WritePipeline::new(registry, nvram));

    let engine = SnapshotEngine::new(pipeline);

    // 2. Create Volume & Write Data
    let vol_id = VolumeId::new();
    let vol = foundry
        .create_volume(vol_id, 1024 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    let secret_msg = Bytes::from("The eagle has landed.");
    vol.write_at(0, secret_msg.clone()).await.unwrap();
    vol.sync().await.unwrap();

    // 3. Take Snapshot
    let manifest_id = engine
        .take_snapshot(vol_id, vol.clone(), Policy::default())
        .await
        .unwrap();
    println!("Snapshot taken: {:?}", manifest_id);

    // 4. Corrupt/Wipe Volume
    vol.write_at(0, Bytes::from(vec![0u8; secret_msg.len()]))
        .await
        .unwrap();

    // 5. Restore
    engine
        .restore_snapshot(vol_id, manifest_id, vol.clone())
        .await
        .unwrap();

    // 6. Verify
    let read_back = vol.read_at(0, secret_msg.len()).await.unwrap();
    assert_eq!(read_back, secret_msg);
}

#[tokio::test]
async fn test_snapshot_large_volume() {
    let _ = tracing_subscriber::fmt::try_init();

    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let registry_path = temp_dir.path().join("registry.db");
    let nvram_path = temp_dir.path().join("nvram.log");

    let registry = CapsuleRegistry::open(registry_path.to_str().unwrap()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_str().unwrap()).unwrap();
    let pipeline = Arc::new(WritePipeline::new(registry, nvram));

    let engine = SnapshotEngine::new(pipeline);

    // Create a 10MB volume
    let vol_id = VolumeId::new();
    let vol_size = 10 * 1024 * 1024;
    let vol = foundry
        .create_volume(vol_id, vol_size, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write pattern at various offsets
    let pattern1 = Bytes::from(vec![0xAA; 4096]);
    let pattern2 = Bytes::from(vec![0xBB; 4096]);
    let pattern3 = Bytes::from(vec![0xCC; 4096]);

    vol.write_at(0, pattern1.clone()).await.unwrap();
    vol.write_at(5 * 1024 * 1024, pattern2.clone())
        .await
        .unwrap();
    vol.write_at(vol_size - 4096, pattern3.clone())
        .await
        .unwrap();
    vol.sync().await.unwrap();

    // Take snapshot
    let manifest_id = engine
        .take_snapshot(vol_id, vol.clone(), Policy::default())
        .await
        .unwrap();

    // Wipe the volume
    let zeros = Bytes::from(vec![0u8; 4096]);
    vol.write_at(0, zeros.clone()).await.unwrap();
    vol.write_at(5 * 1024 * 1024, zeros.clone()).await.unwrap();
    vol.write_at(vol_size - 4096, zeros.clone()).await.unwrap();
    vol.sync().await.unwrap();

    // Restore from snapshot
    engine
        .restore_snapshot(vol_id, manifest_id, vol.clone())
        .await
        .unwrap();

    // Verify all patterns are restored
    let read1 = vol.read_at(0, 4096).await.unwrap();
    assert_eq!(read1, pattern1);

    let read2 = vol.read_at(5 * 1024 * 1024, 4096).await.unwrap();
    assert_eq!(read2, pattern2);

    let read3 = vol.read_at(vol_size - 4096, 4096).await.unwrap();
    assert_eq!(read3, pattern3);
}

#[tokio::test]
async fn test_snapshot_sparse_volume() {
    let _ = tracing_subscriber::fmt::try_init();

    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let registry_path = temp_dir.path().join("registry.db");
    let nvram_path = temp_dir.path().join("nvram.log");

    let registry = CapsuleRegistry::open(registry_path.to_str().unwrap()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_str().unwrap()).unwrap();
    let pipeline = Arc::new(WritePipeline::new(registry, nvram));

    let engine = SnapshotEngine::new(pipeline);

    // Create a sparse volume (100MB with only small writes)
    let vol_id = VolumeId::new();
    let vol = foundry
        .create_volume(vol_id, 100 * 1024 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write only at the beginning and end
    let data_start = Bytes::from(vec![0x11; 1024]);
    let data_end = Bytes::from(vec![0x22; 1024]);

    vol.write_at(0, data_start.clone()).await.unwrap();
    vol.write_at(100 * 1024 * 1024 - 1024, data_end.clone())
        .await
        .unwrap();
    vol.sync().await.unwrap();

    // Take snapshot
    let manifest_id = engine
        .take_snapshot(vol_id, vol.clone(), Policy::default())
        .await
        .unwrap();

    // Create a new volume to restore into
    let new_vol_id = VolumeId::new();
    let new_vol = foundry
        .create_volume(new_vol_id, 1, Some(BackendType::Legacy))
        .await
        .unwrap(); // Start small, will be resized

    // Restore
    engine
        .restore_snapshot(new_vol_id, manifest_id, new_vol.clone())
        .await
        .unwrap();

    // Verify data
    let read_start = new_vol.read_at(0, 1024).await.unwrap();
    assert_eq!(read_start, data_start);

    let read_end = new_vol
        .read_at(100 * 1024 * 1024 - 1024, 1024)
        .await
        .unwrap();
    assert_eq!(read_end, data_end);

    // Verify middle is zeros (sparse)
    let read_middle = new_vol.read_at(50 * 1024 * 1024, 1024).await.unwrap();
    assert_eq!(read_middle, Bytes::from(vec![0u8; 1024]));
}

#[tokio::test]
async fn test_snapshot_with_compression_policy() {
    let _ = tracing_subscriber::fmt::try_init();

    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let registry_path = temp_dir.path().join("registry.db");
    let nvram_path = temp_dir.path().join("nvram.log");

    let registry = CapsuleRegistry::open(registry_path.to_str().unwrap()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_str().unwrap()).unwrap();
    let pipeline = Arc::new(WritePipeline::new(registry, nvram));

    let engine = SnapshotEngine::new(pipeline);

    // Create volume with highly compressible data
    let vol_id = VolumeId::new();
    let vol = foundry
        .create_volume(vol_id, 1024 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write repetitive, compressible data
    let compressible_data = Bytes::from(vec![0x42; 100 * 1024]);
    vol.write_at(0, compressible_data.clone()).await.unwrap();
    vol.sync().await.unwrap();

    // Take snapshot with text-optimized policy (high compression)
    let manifest_id = engine
        .take_snapshot(vol_id, vol.clone(), Policy::text_optimized())
        .await
        .unwrap();

    // Wipe
    vol.write_at(0, Bytes::from(vec![0u8; 100 * 1024]))
        .await
        .unwrap();

    // Restore
    engine
        .restore_snapshot(vol_id, manifest_id, vol.clone())
        .await
        .unwrap();

    // Verify
    let read_back = vol.read_at(0, 100 * 1024).await.unwrap();
    assert_eq!(read_back, compressible_data);
}

#[tokio::test]
async fn test_snapshot_empty_volume() {
    let _ = tracing_subscriber::fmt::try_init();

    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let registry_path = temp_dir.path().join("registry.db");
    let nvram_path = temp_dir.path().join("nvram.log");

    let registry = CapsuleRegistry::open(registry_path.to_str().unwrap()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_str().unwrap()).unwrap();
    let pipeline = Arc::new(WritePipeline::new(registry, nvram));

    let engine = SnapshotEngine::new(pipeline);

    // Create an empty volume
    let vol_id = VolumeId::new();
    let vol = foundry
        .create_volume(vol_id, 64 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Don't write anything - just take a snapshot of zeros
    let manifest_id = engine
        .take_snapshot(vol_id, vol.clone(), Policy::default())
        .await
        .unwrap();

    // Write some data
    vol.write_at(0, Bytes::from(vec![0xFF; 1024]))
        .await
        .unwrap();

    // Restore the empty snapshot
    engine
        .restore_snapshot(vol_id, manifest_id, vol.clone())
        .await
        .unwrap();

    // Verify it's back to zeros
    let read_back = vol.read_at(0, 1024).await.unwrap();
    assert_eq!(read_back, Bytes::from(vec![0u8; 1024]));
}

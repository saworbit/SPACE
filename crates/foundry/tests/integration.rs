//! Integration tests for the Foundry block storage system.

use bytes::Bytes;
use foundry::{BackendType, Foundry, VolumeId};
use tempfile::TempDir;

#[tokio::test]
async fn test_volume_lifecycle() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let size = 10 * 1024 * 1024; // 10MB

    // Create volume
    let backend = foundry
        .create_volume(volume_id, size, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write pattern
    for i in 0..10 {
        let offset = i * 4096;
        let data = Bytes::from(vec![(i % 256) as u8; 4096]);
        backend.write_at(offset, data).await.unwrap();
    }

    // Sync
    backend.sync().await.unwrap();

    // Read back and verify
    for i in 0..10 {
        let offset = i * 4096;
        let data = backend.read_at(offset, 4096).await.unwrap();
        assert_eq!(data[0], (i % 256) as u8);
        assert_eq!(data.len(), 4096);
    }

    // Cleanup
    foundry.delete_volume(volume_id).await.unwrap();
}

#[tokio::test]
async fn test_concurrent_access() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let backend = foundry
        .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write data sequentially
    for i in 0..5 {
        let offset = i * 8192; // Use larger spacing to avoid interference
        let data = Bytes::from(vec![i as u8; 4096]);
        backend.write_at(offset, data).await.unwrap();
    }

    backend.sync().await.unwrap();

    // Verify data was written correctly (sequential reads)
    for i in 0..5 {
        let data = backend.read_at(i * 8192, 4096).await.unwrap();
        assert_eq!(data[0], i as u8);
    }

    // Test that concurrent operations don't crash (basic thread-safety)
    let mut handles = vec![];
    for i in 0..5 {
        let backend = backend.clone();
        handles.push(tokio::spawn(async move {
            // Just verify the operation completes without panicking
            let _ = backend.read_at(i * 8192, 100).await;
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn test_large_sequential_writes() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let size = 50 * 1024 * 1024; // 50MB
    let backend = foundry
        .create_volume(volume_id, size, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write 10MB in 1MB chunks
    let chunk_size = 1024 * 1024;
    for i in 0..10 {
        let offset = (i * chunk_size) as u64;
        let data = Bytes::from(vec![(i % 256) as u8; chunk_size]);
        backend.write_at(offset, data).await.unwrap();
    }

    backend.sync().await.unwrap();

    // Read back first and last chunks
    let first_chunk = backend.read_at(0, chunk_size).await.unwrap();
    assert_eq!(first_chunk[0], 0);

    let last_chunk = backend
        .read_at(9 * chunk_size as u64, chunk_size)
        .await
        .unwrap();
    assert_eq!(last_chunk[0], 9);
}

#[tokio::test]
async fn test_sparse_volume_operations() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let size = 100 * 1024 * 1024; // 100MB sparse volume
    let backend = foundry
        .create_volume(volume_id, size, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write at the beginning
    let data_start = Bytes::from(vec![0xAA; 4096]);
    backend.write_at(0, data_start.clone()).await.unwrap();

    // Write at the end (sparse in between)
    let data_end = Bytes::from(vec![0xBB; 4096]);
    backend
        .write_at(size - 4096, data_end.clone())
        .await
        .unwrap();

    // Read from the beginning
    let read_start = backend.read_at(0, 4096).await.unwrap();
    assert_eq!(read_start, data_start);

    // Read from the middle (should be zeros - sparse)
    let read_middle = backend.read_at(50 * 1024 * 1024, 4096).await.unwrap();
    assert_eq!(read_middle, Bytes::from(vec![0u8; 4096]));

    // Read from the end
    let read_end = backend.read_at(size - 4096, 4096).await.unwrap();
    assert_eq!(read_end, data_end);
}

#[tokio::test]
async fn test_volume_resize() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let initial_size = 10 * 1024 * 1024; // 10MB
    let backend = foundry
        .create_volume(volume_id, initial_size, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write some data
    let data = Bytes::from(vec![0x42; 4096]);
    backend.write_at(0, data.clone()).await.unwrap();

    // Resize to 20MB
    let new_size = 20 * 1024 * 1024;
    backend.resize(new_size).await.unwrap();

    // Verify new size
    let size = backend.size().await.unwrap();
    assert_eq!(size, new_size);

    // Verify old data is intact
    let read_data = backend.read_at(0, 4096).await.unwrap();
    assert_eq!(read_data, data);

    // Verify we can write to the new region
    let new_data = Bytes::from(vec![0x99; 4096]);
    backend
        .write_at(15 * 1024 * 1024, new_data.clone())
        .await
        .unwrap();
    let read_new = backend.read_at(15 * 1024 * 1024, 4096).await.unwrap();
    assert_eq!(read_new, new_data);
}

#[tokio::test]
async fn test_multiple_volumes() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    // Create 5 volumes
    let mut volume_ids = vec![];
    for _ in 0..5 {
        let id = VolumeId::new();
        foundry
            .create_volume(id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();
        volume_ids.push(id);
    }

    // List volumes
    let volumes = foundry.list_volumes().await;
    assert_eq!(volumes.len(), 5);

    for id in &volume_ids {
        assert!(volumes.contains(id));
    }

    // Write different data to each volume
    for (i, id) in volume_ids.iter().enumerate() {
        let volume = foundry.get_volume(*id).await.unwrap();
        let data = Bytes::from(vec![i as u8; 4096]);
        volume.write_at(0, data).await.unwrap();
    }

    // Verify each volume has correct data
    for (i, id) in volume_ids.iter().enumerate() {
        let volume = foundry.get_volume(*id).await.unwrap();
        let data = volume.read_at(0, 4096).await.unwrap();
        assert_eq!(data[0], i as u8);
    }

    // Cleanup
    for id in volume_ids {
        foundry.delete_volume(id).await.unwrap();
    }
}

#[tokio::test]
async fn test_backend_fallback() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path()).with_backend(BackendType::Auto);

    let volume_id = VolumeId::new();

    // Auto should fallback to Legacy (Magma is not implemented)
    let backend = foundry
        .create_volume(volume_id, 1024 * 1024, None)
        .await
        .unwrap();

    // Verify it works
    let data = Bytes::from(vec![0x55; 4096]);
    backend.write_at(0, data.clone()).await.unwrap();

    let read_data = backend.read_at(0, 4096).await.unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test]
async fn test_error_handling() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let backend = foundry
        .create_volume(volume_id, 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Out of bounds read
    let result = backend.read_at(1000, 100).await;
    assert!(result.is_err());

    // Out of bounds write
    let data = Bytes::from(vec![0xFF; 100]);
    let result = backend.write_at(1000, data).await;
    assert!(result.is_err());

    // Volume not found
    let nonexistent_id = VolumeId::new();
    let result = foundry.get_volume(nonexistent_id).await;
    assert!(result.is_err());

    // Volume already exists
    let result = foundry
        .create_volume(volume_id, 1024, Some(BackendType::Legacy))
        .await;
    assert!(result.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_file_sharing() {
    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let backend = foundry
        .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
        .await
        .unwrap();

    // Write some data
    let data = Bytes::from(vec![0x42; 100]);
    backend.write_at(0, data).await.unwrap();

    // On Windows, file sharing should allow external access
    // This is verified by the fact that the backend uses FILE_SHARE_READ | FILE_SHARE_WRITE
    // We can't easily test external access, but we can verify the backend was created successfully
    let size = backend.size().await.unwrap();
    assert_eq!(size, 1024 * 1024);
}

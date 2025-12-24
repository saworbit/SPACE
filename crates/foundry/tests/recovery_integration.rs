use bytes::Bytes;
use foundry::backend::device::DirectIoDevice;
use foundry::backend::magma::MagmaBackend;
use foundry::backend::{VolumeBackend, VolumeId};
use tempfile::TempDir;

#[tokio::test]
async fn test_crash_recovery_scenario() {
    let temp_dir = TempDir::new().unwrap();
    let device_path = temp_dir.path().join("crash_test.img");
    let volume_id = VolumeId::new();
    let size = 10 * 1024 * 1024; // 10MB

    // Simulate application workload
    let workload_data = vec![
        (0, vec![0x01; 4096]),
        (4096, vec![0x02; 8192]),
        (16384, vec![0x03; 4096]),
    ];

    // Phase 1: Initial writes
    {
        let device = DirectIoDevice::open(&device_path).await.unwrap();
        let backend = MagmaBackend::new(volume_id, size, device);
        backend.init(size).await.unwrap();

        for (offset, data) in &workload_data {
            backend
                .write_at(*offset, Bytes::from(data.clone()))
                .await
                .unwrap();
        }

        backend.sync().await.unwrap(); // Checkpoint
    }

    // Phase 2: Simulate crash and recovery
    {
        let device = DirectIoDevice::open(&device_path).await.unwrap();
        let backend = MagmaBackend::open(volume_id, size, device, 4096)
            .await
            .unwrap();

        // Verify all data survived
        for (offset, expected_data) in &workload_data {
            let read_data = backend.read_at(*offset, expected_data.len()).await.unwrap();
            assert_eq!(read_data.as_ref(), expected_data.as_slice());
        }
    }
}

#[tokio::test]
async fn test_multiple_crash_cycles() {
    let temp_dir = TempDir::new().unwrap();
    let device_path = temp_dir.path().join("multi_crash.img");
    let volume_id = VolumeId::new();
    let size = 1024 * 1024;

    for cycle in 0..5 {
        let device = DirectIoDevice::open(&device_path).await.unwrap();
        let backend = if cycle == 0 {
            MagmaBackend::new(volume_id, size, device)
        } else {
            MagmaBackend::open(volume_id, size, device, 4096)
                .await
                .unwrap()
        };

        if cycle == 0 {
            backend.init(size).await.unwrap();
        }

        // Write cycle-specific data
        let data = Bytes::from(vec![cycle as u8; 4096]);
        let offset = (cycle as u64) * 4096;
        backend.write_at(offset, data).await.unwrap();

        backend.sync().await.unwrap();
    }

    // Final recovery - verify all cycles
    let device = DirectIoDevice::open(&device_path).await.unwrap();
    let backend = MagmaBackend::open(volume_id, size, device, 4096)
        .await
        .unwrap();

    for cycle in 0..5 {
        let offset = (cycle as u64) * 4096;
        let data = backend.read_at(offset, 4096).await.unwrap();
        assert_eq!(data, Bytes::from(vec![cycle as u8; 4096]));
    }
}

#[tokio::test]
async fn test_foundry_magma_recovery() {
    use foundry::{BackendType, Foundry};

    let temp_dir = TempDir::new().unwrap();
    let foundry = Foundry::with_data_dir(temp_dir.path());

    let volume_id = VolumeId::new();
    let size = 1024 * 1024;

    // Phase 1: Create volume and write data
    {
        let backend = foundry
            .create_volume(volume_id, size, Some(BackendType::Magma))
            .await
            .unwrap();

        let test_data = Bytes::from(vec![0xAB; 4096]);
        backend.write_at(0, test_data.clone()).await.unwrap();
        backend.sync().await.unwrap();
    }

    // Phase 2: Re-create Foundry instance (simulates restart)
    {
        let foundry = Foundry::with_data_dir(temp_dir.path());

        // Re-open the volume (should recover from checkpoint)
        let backend = foundry
            .create_volume(volume_id, size, Some(BackendType::Magma))
            .await
            .unwrap();

        let read_data = backend.read_at(0, 4096).await.unwrap();
        assert_eq!(read_data, Bytes::from(vec![0xAB; 4096]));
    }
}

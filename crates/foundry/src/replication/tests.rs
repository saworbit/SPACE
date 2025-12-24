//! Tests for chain replication.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::backend::VolumeBackend;
    use crate::{BackendType, Foundry, VolumeId};
    use bytes::Bytes;
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_replication_handshake() {
        // Setup: Create a replica node
        let replica_foundry = Arc::new(Foundry::new());
        let volume_id = VolumeId::new();

        // Create volume on replica
        replica_foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Start replication server in background
        let replica_port = 14421;
        let server_foundry = replica_foundry.clone();
        tokio::spawn(async move {
            start_replication_server(server_foundry, replica_port)
                .await
                .unwrap();
        });

        // Give server time to start
        sleep(Duration::from_millis(100)).await;

        // Connect from primary
        let client = ReplicationClient::connect(
            &format!("127.0.0.1:{}", replica_port),
            volume_id.to_string(),
        )
        .await;

        assert!(client.is_ok(), "Handshake should succeed");
    }

    #[tokio::test]
    async fn test_replication_write() {
        // Setup: Create replica node
        let replica_foundry = Arc::new(Foundry::new());
        let volume_id = VolumeId::new();

        let replica_volume = replica_foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Start replication server
        let replica_port = 14422;
        let server_foundry = replica_foundry.clone();
        tokio::spawn(async move {
            start_replication_server(server_foundry, replica_port)
                .await
                .unwrap();
        });

        sleep(Duration::from_millis(100)).await;

        // Setup: Create primary node
        let primary_foundry = Foundry::new();
        let primary_volume = primary_foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Connect replication client
        let client = ReplicationClient::connect(
            &format!("127.0.0.1:{}", replica_port),
            volume_id.to_string(),
        )
        .await
        .unwrap();

        // Wrap primary with replication
        let replicated = Arc::new(ReplicatedBackend::new(primary_volume, client));

        // Write data through replicated backend
        let test_data = Bytes::from("Hello, Chain Replication!");
        replicated.write_at(0, test_data.clone()).await.unwrap();

        // Give replication time to complete
        sleep(Duration::from_millis(50)).await;

        // Verify data exists on replica by reading directly from replica volume
        let replica_data = replica_volume.read_at(0, test_data.len()).await.unwrap();
        assert_eq!(
            replica_data, test_data,
            "Data should be replicated to replica"
        );

        // Verify data exists on primary
        let primary_data = replicated.read_at(0, test_data.len()).await.unwrap();
        assert_eq!(
            primary_data, test_data,
            "Data should exist on primary"
        );
    }

    #[tokio::test]
    async fn test_replication_multiple_writes() {
        // Setup: Create replica node
        let replica_foundry = Arc::new(Foundry::new());
        let volume_id = VolumeId::new();

        let replica_volume = replica_foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Start replication server
        let replica_port = 14423;
        let server_foundry = replica_foundry.clone();
        tokio::spawn(async move {
            start_replication_server(server_foundry, replica_port)
                .await
                .unwrap();
        });

        sleep(Duration::from_millis(100)).await;

        // Setup: Create primary node
        let primary_foundry = Foundry::new();
        let primary_volume = primary_foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Connect replication client
        let client = ReplicationClient::connect(
            &format!("127.0.0.1:{}", replica_port),
            volume_id.to_string(),
        )
        .await
        .unwrap();

        // Wrap primary with replication
        let replicated = Arc::new(ReplicatedBackend::new(primary_volume, client));

        // Write multiple blocks
        for i in 0..10 {
            let offset = i * 4096;
            let data = Bytes::from(vec![i as u8; 4096]);
            replicated.write_at(offset, data).await.unwrap();
        }

        sleep(Duration::from_millis(100)).await;

        // Verify all blocks on replica
        for i in 0..10 {
            let offset = i * 4096;
            let expected = Bytes::from(vec![i as u8; 4096]);
            let actual = replica_volume.read_at(offset, 4096).await.unwrap();
            assert_eq!(
                actual, expected,
                "Block {} should be replicated correctly",
                i
            );
        }
    }

    #[tokio::test]
    async fn test_replication_handshake_invalid_volume() {
        // Setup: Create a replica node
        let replica_foundry = Arc::new(Foundry::new());

        // Start replication server
        let replica_port = 14424;
        let server_foundry = replica_foundry.clone();
        tokio::spawn(async move {
            start_replication_server(server_foundry, replica_port)
                .await
                .unwrap();
        });

        sleep(Duration::from_millis(100)).await;

        // Try to connect with a volume that doesn't exist
        let nonexistent_volume = VolumeId::new();
        let client = ReplicationClient::connect(
            &format!("127.0.0.1:{}", replica_port),
            nonexistent_volume.to_string(),
        )
        .await;

        assert!(
            client.is_err(),
            "Handshake should fail for nonexistent volume"
        );
    }
}

//! Comprehensive tests for PODMS scaling module

#[cfg(test)]
mod mesh_tests {
    use crate::{MeshNode, NetworkTier};
    use common::podms::ZoneId;
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_mesh_node_lifecycle() {
        let zone = ZoneId::Metro {
            name: "us-west-1a".into(),
        };
        let addr = "127.0.0.1:19000".parse().unwrap();

        let node = MeshNode::new(zone.clone(), addr).await.unwrap();
        assert_eq!(node.zone(), &zone);
        assert!(node.capabilities().has_nvram);
        assert_eq!(
            node.capabilities().network_tier as u8,
            NetworkTier::Standard as u8
        );
    }

    #[tokio::test]
    async fn test_peer_registration_and_lookup() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };
        let addr = "127.0.0.1:19001".parse().unwrap();
        let node = MeshNode::new(zone, addr).await.unwrap();

        // Register multiple peers
        let peer1_id = common::podms::NodeId::new();
        let peer1_addr = "127.0.0.1:19002".parse().unwrap();
        let peer2_id = common::podms::NodeId::new();
        let peer2_addr = "127.0.0.1:19003".parse().unwrap();

        node.register_peer(peer1_id, peer1_addr).await;
        node.register_peer(peer2_id, peer2_addr).await;

        let peers = node.peers.read().await;
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.get(&peer1_id), Some(&peer1_addr));
        assert_eq!(peers.get(&peer2_id), Some(&peer2_addr));
    }

    #[tokio::test]
    async fn test_mirror_segment_requires_registered_peer() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };
        let addr = "127.0.0.1:19004".parse().unwrap();
        let node = MeshNode::new(zone, addr).await.unwrap();

        let unknown_peer = common::podms::NodeId::new();
        let data = b"test segment data";

        // Should fail: peer not registered
        let result = node.mirror_segment(data, unknown_peer).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mirror_segment_basic() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };

        // Create two nodes
        let node1_addr = "127.0.0.1:19005".parse().unwrap();
        let node1 = Arc::new(MeshNode::new(zone.clone(), node1_addr).await.unwrap());

        let node2_addr = "127.0.0.1:19006".parse().unwrap();
        let node2 = Arc::new(MeshNode::new(zone.clone(), node2_addr).await.unwrap());

        // Start node2 to accept mirrors
        node2.start(vec![]).await.unwrap();

        // Give listener time to bind
        sleep(Duration::from_millis(100)).await;

        // Register node2 as peer of node1
        node1.register_peer(node2.id(), node2_addr).await;

        // Mirror data from node1 to node2
        let test_data = b"test segment for mirroring";
        let result = node1.mirror_segment(test_data, node2.id()).await;

        // Should succeed
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod agent_tests {
    use crate::agent::ScalingAgent;
    use crate::MeshNode;
    use common::podms::{Telemetry, ZoneId};
    use common::{CapsuleId, Policy};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_agent_handles_new_capsule_event() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19100".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node);

        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn agent in background
        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send a metro-sync capsule event
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        tx.send(Telemetry::NewCapsule {
            id: capsule_id,
            policy: policy.clone(),
            node_id: None,
        })
        .unwrap();

        // Give agent time to process
        sleep(Duration::from_millis(50)).await;

        // Close channel to shut down agent
        drop(tx);

        // Wait for agent to finish
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_heat_spike() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19101".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node);

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send heat spike event
        tx.send(Telemetry::HeatSpike {
            id: CapsuleId::new(),
            accesses_per_min: 10000,
            node_id: None,
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_capacity_threshold() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19102".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node.clone());

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send capacity threshold event
        tx.send(Telemetry::CapacityThreshold {
            node_id: mesh_node.id(),
            used_bytes: 900_000_000_000,
            total_bytes: 1_000_000_000_000,
            threshold_pct: 0.9,
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_node_degraded() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19103".parse().unwrap();
        let mesh_node = Arc::new(MeshNode::new(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node.clone());

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send node degraded event
        tx.send(Telemetry::NodeDegraded {
            node_id: mesh_node.id(),
            reason: "disk failure detected".into(),
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }
}

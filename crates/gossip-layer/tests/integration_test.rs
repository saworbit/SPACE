//! Integration tests for the gossip layer.

use gossip_layer::{current_timestamp, GossipImpl};
use mesh_core::{GossipConfig, GossipHandler, GossipMessage, LoadReport, NodeRole, Peer};

fn local_peer() -> Peer {
    Peer::new(
        "local-test".to_string(),
        "127.0.0.1:0".parse().unwrap(),
        NodeRole::StorageNode,
    )
}

#[tokio::test]
async fn test_gossip_layer_creation() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let result = GossipImpl::new(config, local_peer(), raft_port).await;
    assert!(result.is_ok(), "Failed to create gossip layer");
}

#[tokio::test]
async fn test_broadcast_heartbeat() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    let msg = GossipMessage::Heartbeat {
        peer_id: "test-peer-123".to_string(),
        raft_port: 9000,
        gossip_addr: Some("127.0.0.1:9000".parse().unwrap()),
        load: LoadReport {
            storage_used_bytes: 1024 * 1024 * 100, // 100 MB
            replication_queue_depth: 0,
        },
        timestamp: current_timestamp(),
    };

    let result = gossip.broadcast("heartbeat", msg).await;
    assert!(result.is_ok(), "Failed to broadcast heartbeat");
}

#[tokio::test]
async fn test_subscribe_to_topic() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    let result = gossip.subscribe("test-topic").await;
    assert!(result.is_ok(), "Failed to subscribe to topic");
}

#[tokio::test]
async fn test_get_stats() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    let stats = gossip.get_stats().await;
    assert!(stats.is_ok(), "Failed to get stats");

    let stats = stats.unwrap();
    assert_eq!(stats.messages_sent, 0);
    assert_eq!(stats.messages_received, 0);
}

#[tokio::test]
async fn test_broadcast_file_uploaded() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    let msg = GossipMessage::FileUploaded {
        path: "/data/test.txt".to_string(),
        size: 1024,
        uploader: "node-1".to_string(),
        hash: "abc123".to_string(),
    };

    let result = gossip.broadcast("data_ops", msg).await;
    assert!(result.is_ok(), "Failed to broadcast file upload");
}

#[tokio::test]
async fn test_broadcast_security_alert() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    let msg = GossipMessage::SecurityAlert {
        severity: "high".to_string(),
        threat: "Unauthorized access attempt detected".to_string(),
        source_peer: "node-1".to_string(),
        timestamp: current_timestamp(),
    };

    let result = gossip.broadcast("security", msg).await;
    assert!(result.is_ok(), "Failed to broadcast security alert");
}

#[tokio::test]
async fn test_multiple_subscriptions() {
    let config = GossipConfig::default();
    let raft_port = local_peer().addr.port();
    let gossip = GossipImpl::new(config, local_peer(), raft_port)
        .await
        .expect("Failed to create gossip layer");

    // Subscribe to multiple topics
    let topics = vec!["updates", "alerts", "metrics"];
    for topic in topics {
        let result = gossip.subscribe(topic).await;
        assert!(result.is_ok(), "Failed to subscribe to {}", topic);
    }
}

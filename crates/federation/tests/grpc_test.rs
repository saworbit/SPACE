//! Tests for gRPC transport layer.
//!
//! These tests verify that Raft messages can be sent and received
//! over gRPC between nodes.

use federation::{start_raft_server, PeerRegistry, RaftServiceImpl, RaftTransportClient};
use raft::prelude::{Message, MessageType};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;

/// Test that RaftService can receive and forward messages to the inbox.
#[tokio::test]
async fn test_raft_service_receive_message() {
    let (inbox_tx, mut inbox_rx) = mpsc::channel(10);
    let addr: SocketAddr = "127.0.0.1:50060".parse().unwrap();

    // Start server
    let service = RaftServiceImpl::new(inbox_tx);
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(federation::rpc::raft_service_server::RaftServiceServer::new(service))
            .serve(addr)
            .await
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create client and send message
    let registry = PeerRegistry::new();
    registry.add_peer(2, format!("http://{}", addr));

    let client = RaftTransportClient::new(Arc::new(registry));

    let msg = Message {
        msg_type: MessageType::MsgHeartbeat as i32,
        from: 1,
        to: 2,
        term: 5,
        ..Default::default()
    };

    // Send message
    client
        .send(2, msg.clone())
        .await
        .expect("failed to send message");

    // Verify message received
    let received = tokio::time::timeout(Duration::from_secs(1), inbox_rx.recv())
        .await
        .expect("timeout waiting for message")
        .expect("inbox closed");

    assert_eq!(received.from, 1);
    assert_eq!(received.to, 2);
    assert_eq!(received.term, 5);
    assert_eq!(received.msg_type, MessageType::MsgHeartbeat as i32);

    println!("✓ Message successfully transmitted over gRPC");

    // Clean up
    server_handle.abort();
}

/// Test peer registry add/get/remove operations.
#[test]
fn test_peer_registry_operations() {
    let registry = PeerRegistry::new();

    // Add peers
    registry.add_peer(1, "http://127.0.0.1:4422".to_string());
    registry.add_peer(2, "http://127.0.0.1:4423".to_string());
    registry.add_peer(3, "http://127.0.0.1:4424".to_string());

    // Verify get
    assert_eq!(
        registry.get_peer(1),
        Some("http://127.0.0.1:4422".to_string())
    );
    assert_eq!(
        registry.get_peer(2),
        Some("http://127.0.0.1:4423".to_string())
    );
    assert_eq!(
        registry.get_peer(3),
        Some("http://127.0.0.1:4424".to_string())
    );

    // Unknown peer
    assert_eq!(registry.get_peer(99), None);

    // Remove peer
    registry.remove_peer(2);
    assert_eq!(registry.get_peer(2), None);

    // Other peers still exist
    assert!(registry.get_peer(1).is_some());
    assert!(registry.get_peer(3).is_some());

    println!("✓ Peer registry operations work correctly");
}

/// Test peer registry initialization from config.
#[test]
fn test_peer_registry_from_config() {
    let registry = PeerRegistry::from_config(&[
        (1, "http://node1:4422"),
        (2, "http://node2:4422"),
        (3, "http://node3:4422"),
    ]);

    assert_eq!(registry.get_peer(1), Some("http://node1:4422".to_string()));
    assert_eq!(registry.get_peer(2), Some("http://node2:4422".to_string()));
    assert_eq!(registry.get_peer(3), Some("http://node3:4422".to_string()));

    println!("✓ Peer registry from_config works correctly");
}

/// Test that multiple messages can be sent in sequence.
#[tokio::test]
async fn test_multiple_messages() {
    let (inbox_tx, mut inbox_rx) = mpsc::channel(10);
    let addr: SocketAddr = "127.0.0.1:50061".parse().unwrap();

    // Start server
    let service = RaftServiceImpl::new(inbox_tx);
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(federation::rpc::raft_service_server::RaftServiceServer::new(service))
            .serve(addr)
            .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create client
    let registry = PeerRegistry::new();
    registry.add_peer(2, format!("http://{}", addr));
    let client = RaftTransportClient::new(Arc::new(registry));

    // Send multiple messages
    for i in 1..=5 {
        let msg = Message {
            msg_type: MessageType::MsgAppend as i32,
            from: 1,
            to: 2,
            term: i,
            log_term: i,
            index: i,
            ..Default::default()
        };

        client.send(2, msg).await.expect("failed to send message");
    }

    // Receive and verify all messages
    for expected_term in 1..=5 {
        let received = tokio::time::timeout(Duration::from_secs(1), inbox_rx.recv())
            .await
            .expect("timeout waiting for message")
            .expect("inbox closed");

        assert_eq!(received.term, expected_term);
        assert_eq!(received.msg_type, MessageType::MsgAppend as i32);
    }

    println!("✓ Multiple messages transmitted successfully");

    server_handle.abort();
}

/// Test connection pooling by sending multiple messages to the same peer.
#[tokio::test]
async fn test_connection_pooling() {
    let (inbox_tx, mut inbox_rx) = mpsc::channel(100);
    let addr: SocketAddr = "127.0.0.1:50062".parse().unwrap();

    // Start server
    let service = RaftServiceImpl::new(inbox_tx);
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(federation::rpc::raft_service_server::RaftServiceServer::new(service))
            .serve(addr)
            .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create client
    let registry = PeerRegistry::new();
    registry.add_peer(2, format!("http://{}", addr));
    let client = RaftTransportClient::new(Arc::new(registry));

    // Send many messages rapidly - connection pooling should reuse the connection
    let start = std::time::Instant::now();
    for i in 1..=20 {
        let msg = Message {
            msg_type: MessageType::MsgHeartbeat as i32,
            from: 1,
            to: 2,
            term: i,
            ..Default::default()
        };

        client.send(2, msg).await.expect("failed to send message");
    }
    let elapsed = start.elapsed();

    // Verify all messages received
    for _ in 1..=20 {
        tokio::time::timeout(Duration::from_secs(1), inbox_rx.recv())
            .await
            .expect("timeout waiting for message")
            .expect("inbox closed");
    }

    println!(
        "✓ Sent 20 messages in {:?} (connection pooling enabled)",
        elapsed
    );
    println!("  Average: {:?} per message", elapsed / 20);

    server_handle.abort();
}

/// Test error handling when peer is not in registry.
#[tokio::test]
async fn test_unknown_peer_error() {
    let registry = PeerRegistry::new();
    let client = RaftTransportClient::new(Arc::new(registry));

    let msg = Message {
        msg_type: MessageType::MsgHeartbeat as i32,
        from: 1,
        to: 999, // Unknown peer
        term: 1,
        ..Default::default()
    };

    let result = client.send(999, msg).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("unknown peer"));

    println!("✓ Unknown peer error handled correctly");
}

/// Test that start_raft_server convenience function works.
#[tokio::test]
async fn test_start_raft_server() {
    let (inbox_tx, mut inbox_rx) = mpsc::channel(10);
    let addr: SocketAddr = "127.0.0.1:50063".parse().unwrap();

    // Start server using convenience function
    let server_handle = tokio::spawn(start_raft_server(addr, inbox_tx));

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a message
    let registry = PeerRegistry::new();
    registry.add_peer(2, format!("http://{}", addr));
    let client = RaftTransportClient::new(Arc::new(registry));

    let msg = Message {
        msg_type: MessageType::MsgRequestVote as i32,
        from: 1,
        to: 2,
        term: 1,
        ..Default::default()
    };

    client.send(2, msg).await.expect("failed to send message");

    // Verify received
    let received = tokio::time::timeout(Duration::from_secs(1), inbox_rx.recv())
        .await
        .expect("timeout")
        .expect("inbox closed");

    assert_eq!(received.msg_type, MessageType::MsgRequestVote as i32);

    println!("✓ start_raft_server convenience function works");

    server_handle.abort();
}

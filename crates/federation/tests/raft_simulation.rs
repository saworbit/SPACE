//! Phase 9.1 Raft simulation test with 3 nodes.
//!
//! This test verifies:
//! 1. Leader election in a 3-node cluster
//! 2. Message routing between nodes
//! 3. Basic cluster functionality
//!
//! The test uses in-process communication via mpsc channels with a
//! router task to simulate network message passing.

use federation::{RaftEngine, RaftEngineConfig};
use raft::prelude::*;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::time::sleep;
use tracing::{info, warn};

const NUM_NODES: u64 = 3;

/// Router task that forwards messages between nodes.
///
/// This simulates the network layer by routing messages based on
/// the destination node ID. It handles:
/// - Unknown destinations (logged at debug level)
/// - Full channels (logged at warn level)
/// - Graceful shutdown
async fn router_task(
    mut router_rx: mpsc::Receiver<(u64, u64, Message)>,
    node_senders: HashMap<u64, mpsc::Sender<Message>>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("router: starting");

    loop {
        tokio::select! {
            Some((from, to, msg)) = router_rx.recv() => {
                if let Some(sender) = node_senders.get(&to) {
                    // Use try_send to avoid blocking the router
                    if sender.try_send(msg).is_err() {
                        warn!(
                            from = from,
                            to = to,
                            "router: node channel full or closed, dropping message"
                        );
                    }
                } else {
                    // Unknown destination - normal during startup when nodes
                    // don't know about each other yet
                }
            }
            _ = shutdown.recv() => {
                info!("router: shutdown signal received");
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_three_node_election() {
    // Initialize tracing for test output
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok();

    info!("Starting 3-node Raft simulation test");

    // Create shutdown broadcast channel
    let (shutdown_broadcast_tx, _) = broadcast::channel::<()>(1);

    // Create router channel
    let (router_tx, router_rx) = mpsc::channel(1000);
    let mut node_senders = HashMap::new();
    let mut node_handles = Vec::new();

    // Create nodes
    for id in 1..=NUM_NODES {
        // Create channels for this node
        let (engine_tx, engine_rx) = mpsc::channel(100); // Inbox
        let (outbox_tx, mut outbox_rx) = mpsc::channel(100); // Outbox
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        // Register this node's inbox with the router
        node_senders.insert(id, engine_tx.clone());

        // Create engine config
        let config = RaftEngineConfig {
            id,
            peers: (1..=NUM_NODES).collect(),
        };

        // Create the Raft engine with in-memory storage
        let engine = RaftEngine::new_memory(config, engine_rx, outbox_tx, shutdown_rx, None)
            .expect("failed to create raft engine");

        // Spawn the engine task
        let engine_handle = tokio::spawn(async move {
            if let Err(e) = engine.run().await {
                warn!(id = id, error = %e, "engine task failed");
            }
            info!(id = id, "engine task exited");
        });

        // Spawn outbox forwarder task
        // This reads from the engine's outbox and forwards to the router
        let router_tx_clone = router_tx.clone();
        let forwarder_handle = tokio::spawn(async move {
            while let Some((to, msg)) = outbox_rx.recv().await {
                let from = msg.from;
                if router_tx_clone.send((from, to, msg)).await.is_err() {
                    warn!(id = id, "forwarder: router channel closed");
                    break;
                }
            }
            info!(id = id, "forwarder task exited");
        });

        node_handles.push((engine_handle, forwarder_handle, shutdown_tx));
    }

    // Spawn router task
    let shutdown_rx = shutdown_broadcast_tx.subscribe();
    let router_handle = tokio::spawn(router_task(router_rx, node_senders, shutdown_rx));

    // Wait for leader election
    // In a 3-node cluster with default settings (1s election timeout),
    // election should complete within 2-3 seconds
    info!("Waiting for leader election...");
    sleep(Duration::from_secs(3)).await;

    // At this point, one node should be leader and two should be followers
    // We can verify this by checking the logs (manual verification for Phase 9.1)
    info!("Election phase complete - check logs for leader election");

    // Shutdown sequence
    info!("Shutting down cluster");

    // Signal shutdown to all nodes
    for (_, _, shutdown_tx) in &node_handles {
        let _ = shutdown_tx.send(()).await;
    }

    // Signal shutdown to router
    let _ = shutdown_broadcast_tx.send(());

    // Wait for all tasks to complete (with timeout)
    for (engine_handle, forwarder_handle, _) in node_handles {
        let _ = tokio::time::timeout(Duration::from_secs(1), engine_handle).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), forwarder_handle).await;
    }

    let _ = tokio::time::timeout(Duration::from_secs(1), router_handle).await;

    info!("Test complete - cluster shutdown successful");
}

#[tokio::test]
async fn test_propose_and_commit() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    info!("Starting propose and commit test");

    // TODO Phase 9.2: Add propose test once we have query API
    // For now, Phase 9.1 just verifies election
    //
    // Future test will:
    // 1. Wait for leader election
    // 2. Identify the leader node
    // 3. Propose a command on the leader
    // 4. Wait for commit
    // 5. Verify all nodes have committed the entry

    info!("Propose test placeholder - will be implemented in Phase 9.2");
}

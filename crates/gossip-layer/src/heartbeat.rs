//! Heartbeat task for periodic gossip messages.

use mesh_core::{GossipHandler, GossipMessage, Peer};
use rand::seq::SliceRandom;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error};

/// Periodic heartbeat task for gossip propagation.
///
/// This task runs in the background and periodically selects a random subset
/// of peers (based on the fanout parameter) to send heartbeat messages to.
///
/// # Arguments
///
/// * `gossip` - The gossip handler implementation
/// * `peers` - Shared list of known peers
/// * `interval_ms` - Heartbeat interval in milliseconds
/// * `fanout` - Number of peers to gossip to each round
pub async fn heartbeat_task(
    gossip: Arc<dyn GossipHandler>,
    peers: Arc<RwLock<Vec<Peer>>>,
    interval_ms: u64,
    fanout: usize,
) {
    let interval = Duration::from_millis(interval_ms);
    let mut interval_timer = tokio::time::interval(interval);

    loop {
        interval_timer.tick().await;

        // Select random peers for this round
        let fanout_peers = {
            let peers_lock = peers.read().await;
            if peers_lock.is_empty() {
                debug!("No peers available for heartbeat");
                continue;
            }

            let mut rng = rand::thread_rng();
            peers_lock
                .choose_multiple(&mut rng, fanout.min(peers_lock.len()))
                .cloned()
                .collect::<Vec<_>>()
        };

        debug!("Sending heartbeat to {} peers", fanout_peers.len());

        // Broadcast heartbeat for each selected peer
        for peer in fanout_peers {
            let timestamp = crate::current_timestamp();
            let msg = GossipMessage::Heartbeat {
                peer_id: peer.id.clone(),
                storage_usage: peer.storage_usage,
                timestamp,
            };

            if let Err(e) = gossip.broadcast("heartbeat", msg).await {
                error!("Failed to send heartbeat for peer {}: {}", peer.id, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::{NodeRole, Result};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use tokio::sync::mpsc;

    // Mock gossip handler for testing
    struct MockGossipHandler {
        broadcast_count: Arc<RwLock<usize>>,
    }

    #[async_trait::async_trait]
    impl GossipHandler for MockGossipHandler {
        async fn broadcast(&self, _topic: &str, _msg: GossipMessage) -> Result<()> {
            *self.broadcast_count.write().await += 1;
            Ok(())
        }

        async fn subscribe(&self, _topic: &str) -> Result<mpsc::Receiver<GossipMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }

        async fn pull_state(&self, _peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
            Ok(HashMap::new())
        }

        async fn get_peers(&self) -> Result<Vec<Peer>> {
            Ok(vec![])
        }

        async fn get_stats(&self) -> Result<mesh_core::GossipStats> {
            Ok(mesh_core::GossipStats::default())
        }
    }

    #[tokio::test]
    async fn test_heartbeat_task() {
        let broadcast_count = Arc::new(RwLock::new(0));
        let gossip = Arc::new(MockGossipHandler {
            broadcast_count: broadcast_count.clone(),
        });

        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let peer = Peer::new("test-peer".to_string(), addr, NodeRole::Viewer);
        let peers = Arc::new(RwLock::new(vec![peer]));

        // Spawn heartbeat task with very short interval
        let gossip_clone = gossip.clone();
        let peers_clone = peers.clone();
        tokio::spawn(async move {
            heartbeat_task(gossip_clone, peers_clone, 10, 1).await;
        });

        // Wait for a few heartbeats
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check that broadcasts occurred
        let count = *broadcast_count.read().await;
        assert!(count > 0, "Expected heartbeats to be sent");
    }
}

//! Heartbeat and liveness tasks for gossip propagation.

use mesh_core::{GossipEvent, GossipHandler, GossipMessage, LoadReport, Peer, PeerStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, warn};

/// Periodic heartbeat task for gossip propagation.
///
/// This task runs in the background and broadcasts a heartbeat for the local
/// node every interval.
pub async fn heartbeat_task(
    gossip: Arc<dyn GossipHandler>,
    local_peer: Peer,
    raft_port: u16,
    load: Arc<RwLock<LoadReport>>,
    interval_ms: u64,
) {
    let interval = Duration::from_millis(interval_ms);
    let mut interval_timer = tokio::time::interval(interval);

    loop {
        interval_timer.tick().await;

        let load_snapshot = load.read().await.clone();
        let timestamp = crate::current_timestamp();
        let msg = GossipMessage::Heartbeat {
            peer_id: local_peer.id.clone(),
            raft_port,
            gossip_addr: Some(local_peer.addr),
            load: load_snapshot,
            timestamp,
        };

        if let Err(e) = gossip.broadcast("heartbeat", msg).await {
            error!("Failed to broadcast heartbeat: {}", e);
        }
    }
}

/// Monitor peers for missed heartbeats and emit NodeLost events.
pub async fn liveness_task(
    peer_store: PeerStore,
    event_tx: broadcast::Sender<GossipEvent>,
    interval_ms: u64,
) {
    let interval = Duration::from_millis(interval_ms);
    let mut interval_timer = tokio::time::interval(interval);
    // Consider peer lost after three missed heartbeats
    let timeout_secs = (interval_ms / 1000).max(1) * 3;

    loop {
        interval_timer.tick().await;
        let now = crate::current_timestamp();

        let peers = peer_store.peers().await;
        for peer in peers {
            if now.saturating_sub(peer.last_gossip_heartbeat) > timeout_secs {
                warn!(peer_id = %peer.id, "peer heartbeat timeout detected");
                peer_store.remove(&peer.id).await;
                let _ = event_tx.send(GossipEvent::NodeLost(peer.id.clone()));
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
        let load = Arc::new(RwLock::new(LoadReport::default()));

        // Spawn heartbeat task with very short interval
        let gossip_clone = gossip.clone();
        tokio::spawn(async move {
            heartbeat_task(gossip_clone, peer, 8080, load, 10).await;
        });

        // Wait for a few heartbeats
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check that broadcasts occurred
        let count = *broadcast_count.read().await;
        assert!(count > 0, "Expected heartbeats to be sent");
    }
}

//! Application state management.

use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, GossipHandler, NodeRole, Peer};
use prometheus::{Encoder, IntCounter, IntGauge, Registry, TextEncoder};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};
use uuid::Uuid;

/// Stored file metadata and content
#[derive(Debug, Clone)]
pub struct StoredFile {
    pub path: String,
    pub content: Vec<u8>,
    pub hash: String,
    pub size: u64,
    pub uploaded_at: u64,
}

/// Commands that can be sent to the mesh layer
#[derive(Debug)]
pub enum MeshCommand {
    /// Broadcast a gossip message
    BroadcastGossip {
        topic: String,
        msg: mesh_core::GossipMessage,
    },
    /// Add a peer to the known peers list
    AddPeer { peer: Peer },
    /// Remove a peer from the known peers list
    RemovePeer { peer_id: String },
    /// Request peer list refresh
    RefreshPeers,
    /// Store a file
    StoreFile { file: StoredFile },
}

/// Shared application state.
///
/// This state is shared across all request handlers and contains
/// references to the storage backend, gossip layer, and mesh coordination.
#[derive(Clone)]
pub struct AppState {
    /// Gossip protocol handler
    pub gossip: Arc<dyn GossipHandler>,

    /// Command channel for mesh operations
    pub mesh_tx: mpsc::UnboundedSender<MeshCommand>,

    /// Cached peer list (updated via gossip)
    pub peers: Arc<RwLock<Vec<Peer>>>,

    /// Prometheus metrics registry
    pub metrics: Arc<Registry>,

    // ── Registered Prometheus metrics ───────────────────────────
    /// Total API requests handled.
    pub api_requests_total: IntCounter,
    /// Total WebSocket messages broadcast.
    pub ws_messages_total: IntCounter,
    /// Current connected peer count (gauge).
    pub connected_peers: IntGauge,
    /// Total gossip messages sent.
    pub gossip_sent_total: IntCounter,
    /// Total files stored via the API.
    pub files_stored_total: IntCounter,

    /// Active WebSocket connections
    pub ws_connections: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>,

    /// Stored files (path -> file data)
    pub files: Arc<RwLock<HashMap<String, StoredFile>>>,

    /// Unique node identifier exposed via the API
    pub node_id: String,

    /// Process start time for uptime reporting
    pub start_time: std::time::Instant,
}

impl AppState {
    /// Create a new application state.
    ///
    /// # Arguments
    ///
    /// * `gossip` - The gossip handler implementation
    pub fn new(gossip: Arc<dyn GossipHandler>) -> Self {
        let (mesh_tx, mesh_rx) = mpsc::unbounded_channel();
        let peers = Arc::new(RwLock::new(Vec::new()));
        let metrics = Arc::new(Registry::new());

        // Register Prometheus metrics
        let api_requests_total =
            IntCounter::new("space_api_requests_total", "Total API requests handled")
                .expect("metric can be created");
        let ws_messages_total = IntCounter::new(
            "space_ws_messages_total",
            "Total WebSocket messages broadcast",
        )
        .expect("metric can be created");
        let connected_peers =
            IntGauge::new("space_connected_peers", "Current connected gossip peers")
                .expect("metric can be created");
        let gossip_sent_total =
            IntCounter::new("space_gossip_sent_total", "Total gossip messages sent")
                .expect("metric can be created");
        let files_stored_total =
            IntCounter::new("space_files_stored_total", "Total files stored via API")
                .expect("metric can be created");

        metrics
            .register(Box::new(api_requests_total.clone()))
            .expect("metric can be registered");
        metrics
            .register(Box::new(ws_messages_total.clone()))
            .expect("metric can be registered");
        metrics
            .register(Box::new(connected_peers.clone()))
            .expect("metric can be registered");
        metrics
            .register(Box::new(gossip_sent_total.clone()))
            .expect("metric can be registered");
        metrics
            .register(Box::new(files_stored_total.clone()))
            .expect("metric can be registered");

        let ws_connections = Arc::new(RwLock::new(HashMap::new()));
        let files = Arc::new(RwLock::new(HashMap::new()));
        let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
        let start_time = std::time::Instant::now();

        // Spawn mesh command handler
        let peers_clone = peers.clone();
        let gossip_clone = gossip.clone();
        let files_clone = files.clone();
        let gauge = connected_peers.clone();
        let gossip_ctr = gossip_sent_total.clone();
        let files_ctr = files_stored_total.clone();
        tokio::spawn(async move {
            Self::mesh_command_handler(
                mesh_rx,
                peers_clone,
                gossip_clone,
                files_clone,
                gauge,
                gossip_ctr,
                files_ctr,
            )
            .await;
        });

        Self {
            gossip,
            mesh_tx,
            peers,
            metrics,
            api_requests_total,
            ws_messages_total,
            connected_peers,
            gossip_sent_total,
            files_stored_total,
            ws_connections,
            files,
            node_id,
            start_time,
        }
    }

    /// Background task for handling mesh commands
    async fn mesh_command_handler(
        mut rx: mpsc::UnboundedReceiver<MeshCommand>,
        peers: Arc<RwLock<Vec<Peer>>>,
        gossip: Arc<dyn GossipHandler>,
        files: Arc<RwLock<HashMap<String, StoredFile>>>,
        connected_peers_gauge: IntGauge,
        gossip_sent_counter: IntCounter,
        files_stored_counter: IntCounter,
    ) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                MeshCommand::BroadcastGossip { topic, msg } => {
                    if let Err(e) = gossip.broadcast(&topic, msg).await {
                        error!("Failed to broadcast gossip: {}", e);
                    } else {
                        gossip_sent_counter.inc();
                    }
                }
                MeshCommand::AddPeer { peer } => {
                    let mut peers_lock = peers.write().await;
                    if !peers_lock.iter().any(|p| p.id == peer.id) {
                        info!("Adding peer: {}", peer.id);
                        peers_lock.push(peer);
                        connected_peers_gauge.set(peers_lock.len() as i64);
                    }
                }
                MeshCommand::RemovePeer { peer_id } => {
                    let mut peers_lock = peers.write().await;
                    peers_lock.retain(|p| p.id != peer_id);
                    connected_peers_gauge.set(peers_lock.len() as i64);
                    info!("Removed peer: {}", peer_id);
                }
                MeshCommand::RefreshPeers => {
                    if let Ok(fresh_peers) = gossip.get_peers().await {
                        let mut peers_lock = peers.write().await;
                        *peers_lock = fresh_peers;
                        connected_peers_gauge.set(peers_lock.len() as i64);
                        info!("Refreshed peer list");
                    }
                }
                MeshCommand::StoreFile { file } => {
                    let mut files_lock = files.write().await;
                    info!("Storing file: {} ({} bytes)", file.path, file.size);
                    files_lock.insert(file.path.clone(), file);
                    files_stored_counter.inc();
                }
            }
        }
    }

    /// Get current metrics as Prometheus text format
    pub fn get_metrics(&self) -> Result<String, Box<dyn std::error::Error>> {
        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = self.metrics.gather();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }

    /// Broadcast a message to all WebSocket connections
    pub async fn broadcast_ws(&self, message: String) {
        let connections = self.ws_connections.read().await;
        for (id, tx) in connections.iter() {
            if let Err(e) = tx.send(message.clone()) {
                error!("Failed to send to WebSocket {}: {}", id, e);
            }
        }
        self.ws_messages_total.inc();
    }
}

impl Default for AppState {
    fn default() -> Self {
        // Create a default gossip implementation for testing
        let config = GossipConfig::default();
        let local_peer = Peer::new(
            "web-interface-default".to_string(),
            "127.0.0.1:0".parse().unwrap(),
            NodeRole::Gateway,
        );
        let raft_port = local_peer.addr.port();
        let gossip = Arc::new(tokio::runtime::Runtime::new().unwrap().block_on(async {
            GossipImpl::new(config, local_peer, raft_port)
                .await
                .unwrap()
        }));
        Self::new(gossip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::{GossipStats, NodeRole, Result};
    use std::net::SocketAddr;

    // Mock gossip handler for testing
    struct MockGossipHandler;

    #[async_trait::async_trait]
    impl GossipHandler for MockGossipHandler {
        async fn broadcast(&self, _topic: &str, _msg: mesh_core::GossipMessage) -> Result<()> {
            Ok(())
        }

        async fn subscribe(
            &self,
            _topic: &str,
        ) -> Result<mpsc::Receiver<mesh_core::GossipMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }

        async fn pull_state(&self, _peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
            Ok(HashMap::new())
        }

        async fn get_peers(&self) -> Result<Vec<Peer>> {
            Ok(vec![])
        }

        async fn get_stats(&self) -> Result<GossipStats> {
            Ok(GossipStats::default())
        }
    }

    #[tokio::test]
    async fn test_add_peer() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let peer = Peer::new("test-peer".to_string(), addr, NodeRole::Viewer);

        state
            .mesh_tx
            .send(MeshCommand::AddPeer { peer: peer.clone() })
            .unwrap();

        // Give some time for the command to process
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let peers = state.peers.read().await;
        assert!(!peers.is_empty());
    }

    #[tokio::test]
    async fn test_broadcast_ws() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let (tx, mut rx) = mpsc::unbounded_channel();

        // Add a mock connection
        state
            .ws_connections
            .write()
            .await
            .insert("test-conn".to_string(), tx);

        // Broadcast message
        state.broadcast_ws("test message".to_string()).await;

        // Check that message was received
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "test message");
    }

    // ── Duplicate peer deduplication ────────────────────────────────

    #[tokio::test]
    async fn test_add_duplicate_peer_is_ignored() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let addr: SocketAddr = "127.0.0.1:9090".parse().unwrap();
        let peer = Peer::new("dup-peer".to_string(), addr, NodeRole::StorageNode);

        // Send the same peer twice
        state
            .mesh_tx
            .send(MeshCommand::AddPeer { peer: peer.clone() })
            .unwrap();
        state
            .mesh_tx
            .send(MeshCommand::AddPeer { peer: peer.clone() })
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let peers = state.peers.read().await;
        assert_eq!(peers.len(), 1, "duplicate peer should not be added twice");
    }

    // ── Remove peer ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_remove_peer() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let addr: SocketAddr = "127.0.0.1:7070".parse().unwrap();
        let peer = Peer::new("remove-me".to_string(), addr, NodeRole::Viewer);

        state.mesh_tx.send(MeshCommand::AddPeer { peer }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Verify peer is present
        assert_eq!(state.peers.read().await.len(), 1);

        state
            .mesh_tx
            .send(MeshCommand::RemovePeer {
                peer_id: "remove-me".to_string(),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert_eq!(state.peers.read().await.len(), 0, "peer should be removed");
    }

    // ── Store file ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_store_file() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let file = StoredFile {
            path: "/data/test.txt".to_string(),
            content: b"hello world".to_vec(),
            hash: "abc123".to_string(),
            size: 11,
            uploaded_at: 1000,
        };

        state.mesh_tx.send(MeshCommand::StoreFile { file }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let files = state.files.read().await;
        assert!(files.contains_key("/data/test.txt"));
        assert_eq!(files["/data/test.txt"].size, 11);
    }

    // ── Prometheus metrics are registered ───────────────────────────

    #[tokio::test]
    async fn test_metrics_registry_not_empty() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let families = state.metrics.gather();
        assert!(
            !families.is_empty(),
            "Prometheus registry should have metrics registered"
        );
    }

    #[tokio::test]
    async fn test_get_metrics_returns_prometheus_text() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let text = state.get_metrics().expect("get_metrics should not fail");
        assert!(
            text.contains("space_api_requests_total"),
            "metrics output should contain api_requests_total"
        );
        assert!(
            text.contains("space_ws_messages_total"),
            "metrics output should contain ws_messages_total"
        );
        assert!(
            text.contains("space_connected_peers"),
            "metrics output should contain connected_peers"
        );
        assert!(
            text.contains("space_gossip_sent_total"),
            "metrics output should contain gossip_sent_total"
        );
        assert!(
            text.contains("space_files_stored_total"),
            "metrics output should contain files_stored_total"
        );
    }

    // ── Metric counters increment on operations ─────────────────────

    #[tokio::test]
    async fn test_ws_messages_counter_increments() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let before = state.ws_messages_total.get();

        // broadcast_ws should increment the counter
        state.broadcast_ws("msg1".to_string()).await;
        state.broadcast_ws("msg2".to_string()).await;

        let after = state.ws_messages_total.get();
        assert_eq!(
            after - before,
            2,
            "ws_messages_total should increment by 2 after two broadcasts"
        );
    }

    #[tokio::test]
    async fn test_connected_peers_gauge_updates_on_add_remove() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let addr: SocketAddr = "127.0.0.1:6060".parse().unwrap();

        // Initial gauge should be 0
        assert_eq!(state.connected_peers.get(), 0);

        // Add a peer → gauge should go to 1
        let peer = Peer::new("gauge-peer".to_string(), addr, NodeRole::Editor);
        state.mesh_tx.send(MeshCommand::AddPeer { peer }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        assert_eq!(state.connected_peers.get(), 1);

        // Remove the peer → gauge should go back to 0
        state
            .mesh_tx
            .send(MeshCommand::RemovePeer {
                peer_id: "gauge-peer".to_string(),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        assert_eq!(state.connected_peers.get(), 0);
    }

    #[tokio::test]
    async fn test_gossip_sent_counter_on_broadcast() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let before = state.gossip_sent_total.get();

        state
            .mesh_tx
            .send(MeshCommand::BroadcastGossip {
                topic: "test-topic".to_string(),
                msg: mesh_core::GossipMessage::Heartbeat {
                    peer_id: "node-1".to_string(),
                    raft_port: 8080,
                    gossip_addr: None,
                    load: mesh_core::LoadReport {
                        storage_used_bytes: 0,
                        replication_queue_depth: 0,
                    },
                    timestamp: 0,
                },
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;

        let after = state.gossip_sent_total.get();
        assert_eq!(
            after - before,
            1,
            "gossip_sent_total should increment after broadcast"
        );
    }

    #[tokio::test]
    async fn test_files_stored_counter_on_store() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let before = state.files_stored_total.get();

        state
            .mesh_tx
            .send(MeshCommand::StoreFile {
                file: StoredFile {
                    path: "/count/test.bin".to_string(),
                    content: vec![0; 8],
                    hash: "deadbeef".to_string(),
                    size: 8,
                    uploaded_at: 0,
                },
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;

        let after = state.files_stored_total.get();
        assert_eq!(
            after - before,
            1,
            "files_stored_total should increment after store"
        );
    }

    // ── Broadcast to multiple WS connections ────────────────────────

    #[tokio::test]
    async fn test_broadcast_ws_to_multiple_connections() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        {
            let mut conns = state.ws_connections.write().await;
            conns.insert("conn-1".to_string(), tx1);
            conns.insert("conn-2".to_string(), tx2);
        }

        state.broadcast_ws("multi-msg".to_string()).await;

        assert_eq!(rx1.recv().await.unwrap(), "multi-msg");
        assert_eq!(rx2.recv().await.unwrap(), "multi-msg");
    }

    // ── Node ID ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_node_id_is_not_empty() {
        let state = AppState::new(Arc::new(MockGossipHandler));
        assert!(!state.node_id.is_empty(), "node_id should not be empty");
    }
}

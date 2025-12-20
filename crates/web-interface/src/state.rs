//! Application state management.

use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, GossipHandler, NodeRole, Peer};
use prometheus::{Encoder, Registry, TextEncoder};
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
        let ws_connections = Arc::new(RwLock::new(HashMap::new()));
        let files = Arc::new(RwLock::new(HashMap::new()));
        let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
        let start_time = std::time::Instant::now();

        // Spawn mesh command handler
        let peers_clone = peers.clone();
        let gossip_clone = gossip.clone();
        let files_clone = files.clone();
        tokio::spawn(async move {
            Self::mesh_command_handler(mesh_rx, peers_clone, gossip_clone, files_clone).await;
        });

        Self {
            gossip,
            mesh_tx,
            peers,
            metrics,
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
    ) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                MeshCommand::BroadcastGossip { topic, msg } => {
                    if let Err(e) = gossip.broadcast(&topic, msg).await {
                        error!("Failed to broadcast gossip: {}", e);
                    }
                }
                MeshCommand::AddPeer { peer } => {
                    let mut peers_lock = peers.write().await;
                    if !peers_lock.iter().any(|p| p.id == peer.id) {
                        info!("Adding peer: {}", peer.id);
                        peers_lock.push(peer);
                    }
                }
                MeshCommand::RemovePeer { peer_id } => {
                    let mut peers_lock = peers.write().await;
                    peers_lock.retain(|p| p.id != peer_id);
                    info!("Removed peer: {}", peer_id);
                }
                MeshCommand::RefreshPeers => {
                    if let Ok(fresh_peers) = gossip.get_peers().await {
                        let mut peers_lock = peers.write().await;
                        *peers_lock = fresh_peers;
                        info!("Refreshed peer list");
                    }
                }
                MeshCommand::StoreFile { file } => {
                    let mut files_lock = files.write().await;
                    info!("Storing file: {} ({} bytes)", file.path, file.size);
                    files_lock.insert(file.path.clone(), file);
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
}

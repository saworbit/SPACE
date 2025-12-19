//! Core types and traits for mesh and gossip protocols.
//!
//! This crate provides the foundational abstractions for the mesh data system,
//! including peer management, gossip message types, storage backend traits,
//! and gossip handler interfaces.
//!
//! # Architecture
//!
//! The core module is designed to be protocol-agnostic, allowing different
//! implementations of the gossip and storage layers to be plugged in.
//!
//! # Examples
//!
//! ```rust
//! use mesh_core::{Peer, NodeRole, GossipMessage};
//!
//! let peer = Peer {
//!     id: "peer-123".to_string(),
//!     addr: "127.0.0.1:8080".parse().unwrap(),
//!     role: NodeRole::Viewer,
//!     storage_usage: 0,
//!     status: "online".to_string(),
//!     gossip_version: 1,
//!     last_gossip_heartbeat: 0,
//! };
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};

/// Errors for core mesh and gossip operations.
#[derive(Error, Debug, Clone)]
pub enum CoreError {
    /// Gossip propagation failed with the given reason
    #[error("Gossip propagation failed: {0}")]
    GossipFailure(String),

    /// Invalid peer identifier or configuration
    #[error("Invalid peer: {0}")]
    InvalidPeer(String),

    /// Storage operation failed
    #[error("Storage error: {0}")]
    StorageError(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Authentication or authorization failed
    #[error("Authentication failed: {0}")]
    AuthError(String),

    /// Network communication error
    #[error("Network error: {0}")]
    NetworkError(String),

    /// Resource not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// Operation timeout
    #[error("Operation timed out: {0}")]
    Timeout(String),

    /// Generic internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type alias for core operations
pub type Result<T> = std::result::Result<T, CoreError>;

/// Enum for node roles with different permission levels.
///
/// This supports Role-Based Access Control (RBAC) in the mesh network.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum NodeRole {
    /// Administrator with full control
    Admin,

    /// Read-only viewer
    #[default]
    Viewer,

    /// Can read and modify data
    Editor,

    /// Storage node that primarily stores data
    StorageNode,

    /// Gateway node for external access
    Gateway,
}

/// Struct representing a mesh peer with gossip state.
///
/// Each peer in the mesh maintains state about its neighbors, including
/// their roles, storage capacity, and gossip protocol version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    /// Unique peer identifier (typically libp2p PeerId)
    pub id: String,

    /// Network address of the peer
    pub addr: SocketAddr,

    /// Role of the peer in the mesh
    pub role: NodeRole,

    /// Current storage usage in bytes
    pub storage_usage: u64,

    /// Current status (e.g., "online", "degraded", "offline")
    pub status: String,

    /// Gossip protocol version for compatibility
    pub gossip_version: u32,

    /// Unix timestamp of last gossip heartbeat
    pub last_gossip_heartbeat: u64,
}

impl Peer {
    /// Create a new peer with default values
    pub fn new(id: String, addr: SocketAddr, role: NodeRole) -> Self {
        Self {
            id,
            addr,
            role,
            storage_usage: 0,
            status: "online".to_string(),
            gossip_version: 1,
            last_gossip_heartbeat: 0,
        }
    }

    /// Check if the peer is currently online based on heartbeat
    pub fn is_online(&self, current_time: u64, timeout_secs: u64) -> bool {
        current_time.saturating_sub(self.last_gossip_heartbeat) < timeout_secs
    }
}

/// Shared peer store used across the control plane components.
#[derive(Clone, Default)]
pub struct PeerStore {
    inner: Arc<RwLock<PeerMap>>,
}

impl PeerStore {
    /// Create an empty peer store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Wrap an existing peer map.
    pub fn from_arc(inner: Arc<RwLock<PeerMap>>) -> Self {
        Self { inner }
    }

    /// Access the underlying map.
    pub fn inner(&self) -> Arc<RwLock<PeerMap>> {
        self.inner.clone()
    }

    /// Insert or update a peer; returns true if the peer was newly inserted.
    pub async fn upsert(&self, peer: Peer) -> bool {
        let mut guard = self.inner.write().await;
        let is_new = !guard.contains_key(&peer.id);
        guard.insert(peer.id.clone(), peer);
        is_new
    }

    /// Remove a peer by id and return it if present.
    pub async fn remove(&self, peer_id: &str) -> Option<Peer> {
        self.inner.write().await.remove(peer_id)
    }

    /// Get a peer by id.
    pub async fn get(&self, peer_id: &str) -> Option<Peer> {
        self.inner.read().await.get(peer_id).cloned()
    }

    /// List peers as a vector.
    pub async fn peers(&self) -> Vec<Peer> {
        self.inner
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>()
    }
}

/// Mapping of peer id to peer descriptor.
pub type PeerMap = HashMap<String, Peer>;

/// Lightweight load report propagated via gossip heartbeats.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LoadReport {
    /// Current storage used by the node in bytes.
    pub storage_used_bytes: u64,
    /// Number of queued replication tasks (best-effort).
    pub replication_queue_depth: u64,
}

/// Gossip-layer events surfaced to the rest of the system.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GossipEvent {
    /// A new peer was observed via gossip.
    NodeDiscovered(Peer),
    /// A peer is considered lost after heartbeat timeout.
    NodeLost(String),
    /// A heartbeat was observed.
    Heartbeat {
        peer_id: String,
        raft_port: u16,
        gossip_addr: Option<SocketAddr>,
        load: LoadReport,
    },
}

/// Enum for gossip message types.
///
/// Gossip messages are propagated epidemically through the mesh network
/// to disseminate state updates, migration intents, and alerts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GossipMessage {
    /// Share peer list updates
    PeerUpdate {
        /// List of peer updates
        peers: Vec<Peer>,
    },

    /// Intent to migrate data from one location to another
    DataMigration {
        /// Source path of data
        path: String,

        /// Target peer ID for migration
        target_peer: String,

        /// Size of data in bytes
        size: u64,
    },

    /// Notification of a data transformation operation
    TransformationNotify {
        /// Path to the transformed data
        path: String,

        /// Operation performed (e.g., "compress", "encrypt", "replicate")
        op: String,

        /// Result status
        status: String,
    },

    /// Security alert broadcast
    SecurityAlert {
        /// Severity level (e.g., "low", "medium", "high", "critical")
        severity: String,

        /// Threat description
        threat: String,

        /// Source peer that detected the threat
        source_peer: String,

        /// Unix timestamp
        timestamp: u64,
    },

    /// File upload notification
    FileUploaded {
        /// Path to the uploaded file
        path: String,

        /// Size in bytes
        size: u64,

        /// Uploader peer ID
        uploader: String,

        /// Content hash for verification
        hash: String,
    },

    /// Heartbeat message
    Heartbeat {
        /// Sender peer ID
        peer_id: String,

        /// Advertised raft/control port for this node
        raft_port: u16,

        /// Optional address for direct control-plane traffic
        gossip_addr: Option<SocketAddr>,

        /// Current load information
        load: LoadReport,

        /// Timestamp
        timestamp: u64,
    },

    /// Generic custom message
    Custom {
        /// Message topic/category
        topic: String,

        /// Payload data
        payload: Vec<u8>,
    },
}

impl GossipMessage {
    /// Get the estimated size of the message in bytes
    pub fn estimated_size(&self) -> usize {
        match self {
            GossipMessage::PeerUpdate { peers } => peers.len() * 256, // Rough estimate
            GossipMessage::DataMigration { path, .. } => path.len() + 100,
            GossipMessage::TransformationNotify { path, op, status } => {
                path.len() + op.len() + status.len() + 50
            }
            GossipMessage::SecurityAlert { threat, .. } => threat.len() + 150,
            GossipMessage::FileUploaded { path, hash, .. } => path.len() + hash.len() + 100,
            GossipMessage::Heartbeat { .. } => 64,
            GossipMessage::Custom { payload, .. } => payload.len() + 50,
        }
    }
}

/// Trait for storage backend implementations.
///
/// Storage backends can be pluggable (e.g., filesystem, S3, database)
/// and integrate with the gossip layer for state propagation.
#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    /// Upload data to the given path
    async fn upload(&mut self, data: Vec<u8>, path: &str) -> Result<()>;

    /// View/retrieve data from the given path
    async fn view(&self, path: &str) -> Result<Vec<u8>>;

    /// List files in a directory
    async fn list(&self, path: &str) -> Result<Vec<String>>;

    /// Delete data at the given path
    async fn delete(&mut self, path: &str) -> Result<()>;

    /// Transform data at the given path with the specified operation
    async fn transform(&mut self, path: &str, op: &str) -> Result<()>;

    /// Get metadata for a file
    async fn metadata(&self, path: &str) -> Result<FileMetadata>;

    /// Hook called when a gossip event occurs
    async fn on_change(&self, msg: GossipMessage) -> Result<()>;
}

/// File metadata information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    /// File path
    pub path: String,

    /// Size in bytes
    pub size: u64,

    /// Content hash (e.g., Blake3)
    pub hash: String,

    /// Creation timestamp
    pub created_at: u64,

    /// Last modified timestamp
    pub modified_at: u64,

    /// MIME type
    pub mime_type: String,
}

/// Trait for gossip handler implementations.
///
/// The gossip handler is responsible for broadcasting messages,
/// subscribing to topics, and synchronizing state with peers.
#[async_trait::async_trait]
pub trait GossipHandler: Send + Sync {
    /// Broadcast a message to a topic
    async fn broadcast(&self, topic: &str, msg: GossipMessage) -> Result<()>;

    /// Subscribe to a topic and receive messages
    async fn subscribe(&self, topic: &str) -> Result<mpsc::Receiver<GossipMessage>>;

    /// Pull state from a specific peer
    async fn pull_state(&self, peer_id: &str) -> Result<HashMap<String, Vec<u8>>>;

    /// Get list of connected peers
    async fn get_peers(&self) -> Result<Vec<Peer>>;

    /// Get gossip statistics
    async fn get_stats(&self) -> Result<GossipStats>;
}

/// Statistics for gossip protocol performance
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct GossipStats {
    /// Total messages sent
    pub messages_sent: u64,

    /// Total messages received
    pub messages_received: u64,

    /// Average convergence time in milliseconds
    pub avg_convergence_ms: f64,

    /// Message duplication rate (0.0 - 1.0)
    pub duplication_rate: f64,

    /// Number of active topics
    pub active_topics: usize,

    /// Number of connected peers
    pub connected_peers: usize,

    /// Bandwidth usage in bytes/second
    pub bandwidth_usage: u64,
}

/// Configuration for gossip protocol
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipConfig {
    /// Number of peers to gossip to in each round (fanout)
    pub fanout: usize,

    /// Heartbeat interval in milliseconds
    pub heartbeat_interval_ms: u64,

    /// Message time-to-live (max hops)
    pub message_ttl: u32,

    /// Maximum message size in bytes
    pub max_message_size: usize,

    /// Enable message compression
    pub enable_compression: bool,

    /// Enable message encryption
    pub enable_encryption: bool,

    /// Signing key for message authentication
    pub signing_key: Vec<u8>,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            fanout: 8,
            heartbeat_interval_ms: 1000,
            message_ttl: 10,
            max_message_size: 4096,
            enable_compression: true,
            enable_encryption: true,
            signing_key: vec![0u8; 32], // Should be loaded from secure config
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_creation() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let peer = Peer::new("test-peer".to_string(), addr, NodeRole::Admin);

        assert_eq!(peer.id, "test-peer");
        assert_eq!(peer.addr, addr);
        assert_eq!(peer.role, NodeRole::Admin);
        assert_eq!(peer.status, "online");
    }

    #[test]
    fn test_peer_is_online() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let mut peer = Peer::new("test-peer".to_string(), addr, NodeRole::Admin);

        peer.last_gossip_heartbeat = 100;
        assert!(peer.is_online(110, 30)); // Within 30 seconds
        assert!(!peer.is_online(200, 30)); // Beyond 30 seconds
    }

    #[test]
    fn test_gossip_message_size_estimation() {
        let msg = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            raft_port: 8080,
            gossip_addr: Some("127.0.0.1:8080".parse().unwrap()),
            load: LoadReport {
                storage_used_bytes: 1024,
                replication_queue_depth: 0,
            },
            timestamp: 12345,
        };

        assert_eq!(msg.estimated_size(), 64);
    }

    #[test]
    fn test_gossip_config_default() {
        let config = GossipConfig::default();

        assert_eq!(config.fanout, 8);
        assert_eq!(config.heartbeat_interval_ms, 1000);
        assert_eq!(config.message_ttl, 10);
        assert!(config.enable_compression);
        assert!(config.enable_encryption);
    }
}

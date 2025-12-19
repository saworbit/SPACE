//! Gossip protocol layer implementation using libp2p.
//!
//! This crate provides epidemic-style message propagation for the mesh network,
//! enabling efficient state synchronization, peer discovery, and event notification
//! across thousands of nodes.
//!
//! # Architecture
//!
//! The gossip layer uses libp2p's gossipsub protocol with the following features:
//! - Probabilistic message propagation (fanout-based)
//! - Message signing and verification (HMAC-SHA256)
//! - TTL-based flood control
//! - Heartbeat-based liveness detection
//! - Topic-based pub/sub
//!
//! # Examples
//!
//! ```rust,no_run
//! use gossip_layer::GossipImpl;
//! use mesh_core::{GossipConfig, GossipHandler, GossipMessage, NodeRole, Peer};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = GossipConfig::default();
//! let local_peer = Peer::new("node-1".to_string(), "127.0.0.1:9000".parse()?, NodeRole::StorageNode);
//! let gossip = GossipImpl::new(config, local_peer).await?;
//!
//! // Broadcast a message
//! let msg = GossipMessage::Heartbeat {
//!     peer_id: "node-1".to_string(),
//!     raft_port: 9000,
//!     gossip_addr: None,
//!     load: mesh_core::LoadReport {
//!         storage_used_bytes: 1024,
//!         replication_queue_depth: 0,
//!     },
//!     timestamp: 12345,
//! };
//! gossip.broadcast("updates", msg).await?;
//! # Ok(())
//! # }
//! ```

use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity},
    identity::Keypair,
    PeerId,
};
use mesh_core::{
    CoreError, GossipConfig, GossipEvent, GossipHandler, GossipMessage, GossipStats, LoadReport,
    NodeRole, Peer, PeerStore, Result,
};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info};

mod behaviour;
mod heartbeat;
mod message;

pub use behaviour::GossipBehaviour;
pub use heartbeat::heartbeat_task;
pub use message::{verify_message, SignedMessage};

/// Implementation of the gossip protocol using libp2p.
#[derive(Clone)]
pub struct GossipImpl {
    /// Local peer ID
    peer_id: PeerId,

    /// Local peer descriptor used for heartbeats
    local_peer: Peer,

    /// Gossip configuration
    config: Arc<GossipConfig>,

    /// Message channels for each subscribed topic
    #[allow(dead_code)]
    topic_channels: Arc<RwLock<HashMap<String, mpsc::Sender<GossipMessage>>>>,

    /// Known peers
    #[allow(dead_code)]
    peers: Arc<RwLock<Vec<Peer>>>,

    /// Gossip statistics
    #[allow(dead_code)]
    stats: Arc<RwLock<GossipStats>>,

    /// Shared peer store across control plane components
    peer_store: PeerStore,

    /// Heartbeat load snapshot
    load: Arc<RwLock<LoadReport>>,

    /// Command channel sender
    cmd_tx: mpsc::UnboundedSender<GossipCommand>,

    /// Event broadcast channel for consumers (registry, web UI)
    event_tx: broadcast::Sender<GossipEvent>,
}

/// Internal commands for the gossip handler
#[derive(Debug)]
enum GossipCommand {
    Broadcast {
        topic: String,
        message: GossipMessage,
    },
    Subscribe {
        topic: String,
        tx: mpsc::Sender<GossipMessage>,
    },
    GetPeers {
        tx: tokio::sync::oneshot::Sender<Vec<Peer>>,
    },
    GetStats {
        tx: tokio::sync::oneshot::Sender<GossipStats>,
    },
    Inject {
        topic: String,
        message: GossipMessage,
    },
}

impl GossipImpl {
    /// Create a new gossip implementation with a fresh peer store.
    ///
    /// This initializes the libp2p swarm with gossipsub behavior and starts
    /// the background event loop.
    pub async fn new(config: GossipConfig, local_peer: Peer) -> Result<Self> {
        Self::with_peer_store(config, local_peer, PeerStore::new()).await
    }

    /// Create a new gossip implementation that reuses an existing peer store.
    pub async fn with_peer_store(
        config: GossipConfig,
        local_peer: Peer,
        peer_store: PeerStore,
    ) -> Result<Self> {
        let keypair = Keypair::generate_ed25519();
        let peer_id = PeerId::from(keypair.public());

        info!("Starting gossip layer with peer ID: {}", peer_id);

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let topic_channels = Arc::new(RwLock::new(HashMap::new()));
        let peers = Arc::new(RwLock::new(Vec::new()));
        let stats = Arc::new(RwLock::new(GossipStats::default()));
        let load = Arc::new(RwLock::new(LoadReport::default()));
        let (event_tx, _) = broadcast::channel(128);

        // Create gossipsub config
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(std::time::Duration::from_millis(
                config.heartbeat_interval_ms,
            ))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .mesh_n(config.fanout)
            .mesh_n_low(config.fanout / 2)
            .mesh_n_high(config.fanout * 2)
            .max_transmit_size(config.max_message_size)
            .build()
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        // Create message authenticity
        let message_authenticity = MessageAuthenticity::Signed(keypair.clone());

        // Build gossipsub
        let mut gossipsub: gossipsub::Behaviour =
            gossipsub::Behaviour::new(message_authenticity, gossipsub_config)
                .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        // Subscribe to default topics
        let default_topic = IdentTopic::new("updates");
        gossipsub
            .subscribe(&default_topic)
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        info!("Gossip layer initialized successfully");

        // Ensure the local peer is present in the shared store
        peer_store.upsert(local_peer.clone()).await;

        // Spawn event loop
        let topic_channels_clone = topic_channels.clone();
        let stats_clone = stats.clone();
        let peers_clone = peers.clone();
        let peer_store_clone = peer_store.clone();
        let event_tx_clone = event_tx.clone();

        tokio::spawn(async move {
            Self::event_loop(
                cmd_rx,
                gossipsub,
                topic_channels_clone,
                stats_clone,
                peers_clone,
                peer_store_clone,
                event_tx_clone,
            )
            .await;
        });

        // Spawn heartbeat and liveness monitors
        let heartbeat_impl = GossipImpl {
            peer_id: peer_id.clone(),
            local_peer: local_peer.clone(),
            config: Arc::new(config.clone()),
            topic_channels: topic_channels.clone(),
            peers: peers.clone(),
            stats: stats.clone(),
            peer_store: peer_store.clone(),
            load: load.clone(),
            cmd_tx: cmd_tx.clone(),
            event_tx: event_tx.clone(),
        };

        let heartbeat_task_gossip: Arc<dyn GossipHandler> = Arc::new(heartbeat_impl.clone());
        tokio::spawn(heartbeat::heartbeat_task(
            heartbeat_task_gossip,
            local_peer.clone(),
            local_peer.addr.port(),
            load.clone(),
            config.heartbeat_interval_ms,
        ));

        tokio::spawn(heartbeat::liveness_task(
            peer_store.clone(),
            event_tx.clone(),
            config.heartbeat_interval_ms,
        ));

        Ok(Self {
            peer_id,
            local_peer,
            config: Arc::new(config),
            topic_channels,
            peers,
            stats,
            peer_store,
            load,
            cmd_tx,
            event_tx,
        })
    }

    /// Background event loop for processing gossip events
    async fn event_loop(
        mut cmd_rx: mpsc::UnboundedReceiver<GossipCommand>,
        mut gossipsub: gossipsub::Behaviour,
        topic_channels: Arc<RwLock<HashMap<String, mpsc::Sender<GossipMessage>>>>,
        stats: Arc<RwLock<GossipStats>>,
        peers: Arc<RwLock<Vec<Peer>>>,
        peer_store: PeerStore,
        event_tx: broadcast::Sender<GossipEvent>,
    ) {
        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        GossipCommand::Broadcast { topic, message } => {
                            let topic_obj = IdentTopic::new(topic.clone());

                            // Serialize message
                            let serialized = match bincode::serialize(&message) {
                                Ok(data) => data,
                                Err(e) => {
                                    error!("Failed to serialize message: {}", e);
                                    continue;
                                }
                            };

                            // Publish to topic
                            if let Err(e) = gossipsub.publish(topic_obj, serialized) {
                                error!("Failed to publish to topic {}: {}", topic, e);
                            } else {
                                debug!("Published message to topic: {}", topic);
                                stats.write().await.messages_sent += 1;
                                Self::handle_local_message(&topic, &message, &topic_channels, &peer_store, &event_tx).await;
                            }
                        }
                        GossipCommand::Subscribe { topic, tx } => {
                            let topic_obj = IdentTopic::new(topic.clone());

                            // Subscribe to topic
                            if let Err(e) = gossipsub.subscribe(&topic_obj) {
                                error!("Failed to subscribe to topic {}: {}", topic, e);
                            } else {
                                debug!("Subscribed to topic: {}", topic);
                                topic_channels.write().await.insert(topic, tx);
                                stats.write().await.active_topics += 1;
                            }
                        }
                        GossipCommand::GetPeers { tx } => {
                            let peer_list = peer_store.peers().await;
                            let _ = tx.send(peer_list);
                        }
                        GossipCommand::GetStats { tx } => {
                            let stats_copy = stats.read().await.clone();
                            let _ = tx.send(stats_copy);
                        }
                        GossipCommand::Inject { topic, message } => {
                            Self::handle_local_message(&topic, &message, &topic_channels, &peer_store, &event_tx).await;
                        }
                    }
                }
                // TODO: In a full libp2p swarm implementation, we would poll swarm events here:
                // event = swarm.select_next_some() => {
                //     match event {
                //         SwarmEvent::Behaviour(GossipBehaviourEvent::Gossipsub(event)) => {
                //             handle_gossipsub_event(event, topic_channels, stats, peers).await;
                //         }
                //         SwarmEvent::NewListenAddr { address, .. } => {
                //             info!("Listening on {}", address);
                //         }
                //         SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                //             debug!("Connection established with {}", peer_id);
                //             update_peer_list(peer_id, peers).await;
                //         }
                //         _ => {}
                //     }
                // }
                //
                // For this implementation, gossipsub is used in a simplified mode
                // without a full swarm. This works for the current use case but
                // should be upgraded to full libp2p swarm for production.
            }
        }
    }

    /// Get the local peer ID
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Get the shared peer store.
    pub fn peer_store(&self) -> PeerStore {
        self.peer_store.clone()
    }

    /// Subscribe to gossip events (peer joins/loss/heartbeats).
    pub fn subscribe_events(&self) -> broadcast::Receiver<GossipEvent> {
        self.event_tx.subscribe()
    }

    /// Update the local load report that will be advertised in heartbeats.
    pub async fn set_load_report(&self, load: LoadReport) {
        let mut guard = self.load.write().await;
        *guard = load;
    }

    async fn handle_local_message(
        topic: &str,
        message: &GossipMessage,
        topic_channels: &Arc<RwLock<HashMap<String, mpsc::Sender<GossipMessage>>>>,
        peer_store: &PeerStore,
        event_tx: &broadcast::Sender<GossipEvent>,
    ) {
        if let Some(tx) = topic_channels.read().await.get(topic).cloned() {
            let _ = tx.send(message.clone()).await;
        }

        if let GossipMessage::Heartbeat {
            peer_id,
            raft_port,
            gossip_addr,
            load,
            timestamp,
        } = message
        {
            let mut peer = peer_store
                .get(peer_id)
                .await
                .unwrap_or_else(|| {
                    let addr = gossip_addr.unwrap_or_else(|| {
                        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), *raft_port)
                    });
                    Peer::new(peer_id.clone(), addr, NodeRole::StorageNode)
                });

            if let Some(addr) = gossip_addr {
                peer.addr = *addr;
            } else if peer.addr.port() != *raft_port {
                peer.addr = SocketAddr::new(peer.addr.ip(), *raft_port);
            }

            peer.storage_usage = load.storage_used_bytes;
            peer.last_gossip_heartbeat = *timestamp;
            let inserted = peer_store.upsert(peer.clone()).await;

            let _ = event_tx.send(GossipEvent::Heartbeat {
                peer_id: peer_id.clone(),
                raft_port: *raft_port,
                load: load.clone(),
            });
            if inserted {
                let _ = event_tx.send(GossipEvent::NodeDiscovered(peer));
            }
        }
    }

    /// Get current configuration
    pub fn config(&self) -> &GossipConfig {
        &self.config
    }
}

#[async_trait::async_trait]
impl GossipHandler for GossipImpl {
    async fn broadcast(&self, topic: &str, msg: GossipMessage) -> Result<()> {
        self.cmd_tx
            .send(GossipCommand::Broadcast {
                topic: topic.to_string(),
                message: msg,
            })
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<mpsc::Receiver<GossipMessage>> {
        let (tx, rx) = mpsc::channel(64);

        self.cmd_tx
            .send(GossipCommand::Subscribe {
                topic: topic.to_string(),
                tx,
            })
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        Ok(rx)
    }

    async fn pull_state(&self, peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
        // For now, return empty state
        // In a full implementation, this would open a direct stream to the peer
        debug!("pull_state called for peer: {}", peer_id);
        Ok(HashMap::new())
    }

    async fn get_peers(&self) -> Result<Vec<Peer>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.cmd_tx
            .send(GossipCommand::GetPeers { tx })
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        rx.await
            .map_err(|e| CoreError::GossipFailure(e.to_string()))
    }

    async fn get_stats(&self) -> Result<GossipStats> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.cmd_tx
            .send(GossipCommand::GetStats { tx })
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        rx.await
            .map_err(|e| CoreError::GossipFailure(e.to_string()))
    }
}

/// Utility function to get current Unix timestamp
pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::{GossipConfig, NodeRole};

    fn local_peer() -> Peer {
        Peer::new(
            "local-test".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            NodeRole::StorageNode,
        )
    }

    #[tokio::test]
    async fn test_gossip_creation() {
        let config = GossipConfig::default();
        let result = GossipImpl::new(config, local_peer()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast() {
        let config = GossipConfig::default();
        let gossip = GossipImpl::new(config, local_peer()).await.unwrap();

        let msg = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            raft_port: 9000,
            gossip_addr: Some("127.0.0.1:9000".parse().unwrap()),
            load: LoadReport {
                storage_used_bytes: 1024,
                replication_queue_depth: 0,
            },
            timestamp: current_timestamp(),
        };

        let result = gossip.broadcast("test-topic", msg).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_current_timestamp() {
        let ts = current_timestamp();
        assert!(ts > 0);
    }
}

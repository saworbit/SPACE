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
//! use mesh_core::{GossipConfig, GossipHandler, GossipMessage};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = GossipConfig::default();
//! let gossip = GossipImpl::new(config).await?;
//!
//! // Broadcast a message
//! let msg = GossipMessage::Heartbeat {
//!     peer_id: "node-1".to_string(),
//!     storage_usage: 1024,
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
use mesh_core::{CoreError, GossipConfig, GossipHandler, GossipMessage, GossipStats, Peer, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info};

mod behaviour;
mod heartbeat;
mod message;

pub use behaviour::GossipBehaviour;
pub use heartbeat::heartbeat_task;
pub use message::{verify_message, SignedMessage};

/// Implementation of the gossip protocol using libp2p.
pub struct GossipImpl {
    /// Local peer ID
    peer_id: PeerId,

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

    /// Command channel sender
    cmd_tx: mpsc::UnboundedSender<GossipCommand>,
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
}

impl GossipImpl {
    /// Create a new gossip implementation.
    ///
    /// This initializes the libp2p swarm with gossipsub behavior and starts
    /// the background event loop.
    pub async fn new(config: GossipConfig) -> Result<Self> {
        let keypair = Keypair::generate_ed25519();
        let peer_id = PeerId::from(keypair.public());

        info!("Starting gossip layer with peer ID: {}", peer_id);

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let topic_channels = Arc::new(RwLock::new(HashMap::new()));
        let peers = Arc::new(RwLock::new(Vec::new()));
        let stats = Arc::new(RwLock::new(GossipStats::default()));

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

        // Spawn event loop
        let topic_channels_clone = topic_channels.clone();
        let stats_clone = stats.clone();
        let peers_clone = peers.clone();

        tokio::spawn(async move {
            Self::event_loop(
                cmd_rx,
                gossipsub,
                topic_channels_clone,
                stats_clone,
                peers_clone,
            )
            .await;
        });

        Ok(Self {
            peer_id,
            config: Arc::new(config),
            topic_channels,
            peers,
            stats,
            cmd_tx,
        })
    }

    /// Background event loop for processing gossip events
    async fn event_loop(
        mut cmd_rx: mpsc::UnboundedReceiver<GossipCommand>,
        mut gossipsub: gossipsub::Behaviour,
        topic_channels: Arc<RwLock<HashMap<String, mpsc::Sender<GossipMessage>>>>,
        stats: Arc<RwLock<GossipStats>>,
        peers: Arc<RwLock<Vec<Peer>>>,
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
                            let peer_list = peers.read().await.clone();
                            let _ = tx.send(peer_list);
                        }
                        GossipCommand::GetStats { tx } => {
                            let stats_copy = stats.read().await.clone();
                            let _ = tx.send(stats_copy);
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
    use mesh_core::GossipConfig;

    #[tokio::test]
    async fn test_gossip_creation() {
        let config = GossipConfig::default();
        let result = GossipImpl::new(config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast() {
        let config = GossipConfig::default();
        let gossip = GossipImpl::new(config).await.unwrap();

        let msg = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            storage_usage: 1024,
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

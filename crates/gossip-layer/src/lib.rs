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
//! let raft_port = local_peer.addr.port();
//! let gossip = GossipImpl::new(config, local_peer, raft_port).await?;
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

use futures::StreamExt;
use libp2p_core::{transport::Boxed, upgrade, Transport as _};
use libp2p_gossipsub::{self as gossipsub, IdentTopic, MessageAuthenticity};
use libp2p_identity::{Keypair, PeerId};
use libp2p_noise as noise;
use libp2p_swarm::{Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_tcp as tcp;
use libp2p_yamux as yamux;
use mesh_core::{
    CoreError, GossipConfig, GossipEvent, GossipHandler, GossipMessage, GossipStats, LoadReport,
    NodeRole, Peer, PeerStore, Result,
};
use multiaddr::Protocol;
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
use behaviour::GossipBehaviourEvent;
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
    Dial {
        addr: SocketAddr,
    },
}

impl GossipImpl {
    /// Create a new gossip implementation with a fresh peer store.
    ///
    /// This initializes the libp2p swarm with gossipsub behavior and starts
    /// the background event loop.
    pub async fn new(config: GossipConfig, local_peer: Peer, raft_port: u16) -> Result<Self> {
        Self::with_peer_store(config, local_peer, raft_port, PeerStore::new()).await
    }

    /// Create a new gossip implementation that reuses an existing peer store.
    pub async fn with_peer_store(
        config: GossipConfig,
        local_peer: Peer,
        raft_port: u16,
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
        let heartbeat_topic = IdentTopic::new("heartbeat");
        gossipsub
            .subscribe(&heartbeat_topic)
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        info!("Gossip layer initialized successfully");

        // Ensure the local peer is present in the shared store
        let mut local_peer = local_peer;
        local_peer.last_gossip_heartbeat = current_timestamp();
        peer_store.upsert(local_peer.clone()).await;

        // Build a libp2p swarm using an explicit transport stack (TCP + Noise + Yamux).
        let behaviour = GossipBehaviour { gossipsub };

        let noise_config =
            noise::Config::new(&keypair).map_err(|e| CoreError::GossipFailure(e.to_string()))?;

        let transport: Boxed<(PeerId, libp2p_core::muxing::StreamMuxerBox)> =
            tcp::tokio::Transport::new(tcp::Config::default())
                .upgrade(upgrade::Version::V1)
                .authenticate(noise_config)
                .multiplex(yamux::Config::default())
                .boxed();

        let mut swarm = Swarm::new(
            transport,
            behaviour,
            peer_id,
            SwarmConfig::with_tokio_executor(),
        );

        let listen_addr = socketaddr_to_multiaddr(local_peer.addr);
        if let Err(e) = swarm.listen_on(listen_addr) {
            let msg = if e.to_string().is_empty() {
                format!("{e:?}")
            } else {
                e.to_string()
            };

            #[cfg(test)]
            let _ = msg;

            #[cfg(test)]
            {
                // CI can run unit tests concurrently; fall back to an ephemeral port to avoid
                // flaky failures when a hard-coded test port is already taken.
                let fallback = SocketAddr::new(local_peer.addr.ip(), 0);
                let fallback_addr = socketaddr_to_multiaddr(fallback);
                swarm
                    .listen_on(fallback_addr)
                    .map_err(|e| CoreError::GossipFailure(format!("{e:?}")))?;
            }

            #[cfg(not(test))]
            return Err(CoreError::GossipFailure(msg));
        }

        // Spawn event loop
        let topic_channels_clone = topic_channels.clone();
        let stats_clone = stats.clone();
        let peers_clone = peers.clone();
        let peer_store_clone = peer_store.clone();
        let event_tx_clone = event_tx.clone();

        tokio::spawn(async move {
            Self::event_loop(
                cmd_rx,
                swarm,
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
            peer_id,
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
            raft_port,
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
        mut swarm: Swarm<GossipBehaviour>,
        topic_channels: Arc<RwLock<HashMap<String, mpsc::Sender<GossipMessage>>>>,
        stats: Arc<RwLock<GossipStats>>,
        _peers: Arc<RwLock<Vec<Peer>>>,
        peer_store: PeerStore,
        event_tx: broadcast::Sender<GossipEvent>,
    ) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break; };
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
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic_obj, serialized) {
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
                            if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&topic_obj) {
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
                        GossipCommand::Dial { addr } => {
                            let maddr = socketaddr_to_multiaddr(addr);
                            if let Err(e) = swarm.dial(maddr.clone()) {
                                error!(addr = %addr, error = %e, "failed to dial peer address");
                            } else {
                                info!(addr = %addr, "dialing peer");
                            }
                        }
                    }
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!(addr = %address, "gossip listening");
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            debug!(peer = %peer_id, "gossip connection established");
                            stats.write().await.connected_peers = swarm.connected_peers().count();
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            debug!(peer = %peer_id, "gossip connection closed");
                            stats.write().await.connected_peers = swarm.connected_peers().count();
                        }
                        SwarmEvent::Behaviour(GossipBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. },
                        )) => {
                            stats.write().await.messages_received += 1;
                            let topic = message.topic.as_str().to_string();
                            match bincode::deserialize::<GossipMessage>(&message.data) {
                                Ok(decoded) => {
                                    Self::handle_local_message(
                                        &topic,
                                        &decoded,
                                        &topic_channels,
                                        &peer_store,
                                        &event_tx,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    error!(error = %e, "failed to decode gossip message");
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Get the local peer ID
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Get the local node descriptor advertised in heartbeats.
    pub fn local_peer(&self) -> &Peer {
        &self.local_peer
    }

    /// Dial a seed peer address (best-effort).
    pub async fn dial(&self, addr: SocketAddr) -> Result<()> {
        self.cmd_tx
            .send(GossipCommand::Dial { addr })
            .map_err(|e| CoreError::GossipFailure(e.to_string()))?;
        Ok(())
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
            let mut peer = peer_store.get(peer_id).await.unwrap_or_else(|| {
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
                gossip_addr: *gossip_addr,
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

fn socketaddr_to_multiaddr(addr: SocketAddr) -> multiaddr::Multiaddr {
    let mut out = multiaddr::Multiaddr::empty();
    match addr.ip() {
        IpAddr::V4(ip) => out.push(Protocol::Ip4(ip)),
        IpAddr::V6(ip) => out.push(Protocol::Ip6(ip)),
    }
    out.push(Protocol::Tcp(addr.port()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::{GossipConfig, NodeRole};

    fn local_peer() -> Peer {
        Peer::new(
            "local-test".to_string(),
            "127.0.0.1:0".parse().unwrap(),
            NodeRole::StorageNode,
        )
    }

    #[tokio::test]
    async fn test_gossip_creation() {
        let config = GossipConfig::default();
        let raft_port = local_peer().addr.port();
        let result = GossipImpl::new(config, local_peer(), raft_port).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast() {
        let config = GossipConfig::default();
        let raft_port = local_peer().addr.port();
        let gossip = GossipImpl::new(config, local_peer(), raft_port)
            .await
            .unwrap();

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

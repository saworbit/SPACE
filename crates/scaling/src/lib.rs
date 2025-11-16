//! PODMS Scaling Module - Metro-Sync Replication and Mesh Networking
//!
//! This module implements the core distribution capabilities for PODMS Step 2:
//! - Mesh networking with gossip-based peer discovery
//! - RDMA mocks for zero-copy data transport
//! - Metro-sync replication for zero-RPO policies
//! - Scaling agents for autonomous telemetry-driven migrations

use anyhow::{anyhow, Result};
use common::podms::{NodeId, ZoneId};
use common::SegmentId;
#[cfg(feature = "phase4")]
use common::CapsuleId;
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

#[cfg(feature = "phase4")]
use raft_rs::{RaftCluster, RaftClusterConfig, ShardKey};

pub mod agent;
pub mod batch_queue;
pub mod compiler;
pub mod replication;
#[cfg(test)]
mod tests;

// Re-export key compiler types for external use
pub use compiler::{
    EvacuationUrgency, MeshState, NodeInfo, PolicyCompiler, ReplicationStrategy, ScalingAction,
};

// Re-export replication types for external use
pub use replication::{ContentStore, ReplicationFrame, ReplicationHandler};

// Re-export batch queue types for external use
pub use batch_queue::{BatchItem, BatchQueue, BatchQueueSender, QueueStats};

/// Mesh node capabilities for disaggregated access.
/// Nodes advertise their capabilities (e.g., GPU, NVRAM, network tier) via gossip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeCapabilities {
    pub has_nvram: bool,
    pub has_gpu: bool,
    pub network_tier: NetworkTier,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum NetworkTier {
    Standard, // <10ms metro latency
    Premium,  // <2ms with RDMA
    Edge,     // >50ms edge sites
}

impl Default for NodeCapabilities {
    fn default() -> Self {
        Self {
            has_nvram: true,
            has_gpu: false,
            network_tier: NetworkTier::Standard,
            available_bytes: 1_000_000_000_000, // 1TB default
        }
    }
}

/// Metadata shard description used by Phase 4 federation.
#[cfg(feature = "phase4")]
#[derive(Debug, Clone)]
pub struct MetadataShard {
    pub shard_id: u64,
    pub owner: NodeId,
    pub zone: ZoneId,
}

/// Mesh node for PODMS distribution.
/// Handles peer discovery via gossip and provides zero-copy segment mirroring.
pub struct MeshNode<C: ContentStore> {
    id: NodeId,
    zone: ZoneId,
    capabilities: NodeCapabilities,
    /// Peer registry: NodeId -> SocketAddr
    /// For Step 2, peers are manually registered
    /// Step 3 will add gossip-based auto-discovery
    peers: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    /// Local listen address for mirroring
    listen_addr: SocketAddr,
    /// Replication handler for inbound segment mirroring
    replication_handler: Arc<ReplicationHandler<C>>,
}

impl<C: ContentStore + 'static> MeshNode<C> {
    /// Create a new mesh node in the specified zone.
    /// Initializes gossip discovery but doesn't join until `start()` is called.
    pub async fn new(
        zone: ZoneId,
        listen_addr: SocketAddr,
        content_store: Arc<RwLock<C>>,
        nvram_log: Arc<RwLock<NvramLog>>,
        key_manager: Arc<RwLock<KeyManager>>,
    ) -> Result<Self> {
        let id = NodeId::new();
        let capabilities = NodeCapabilities::default();

        info!(
            node_id = %id,
            zone = %zone,
            listen_addr = %listen_addr,
            "creating mesh node with replication support"
        );

        let replication_handler = Arc::new(ReplicationHandler::new(
            content_store,
            nvram_log,
            key_manager,
        ));

        Ok(Self {
            id,
            zone,
            capabilities,
            peers: Arc::new(RwLock::new(HashMap::new())),
            listen_addr,
            replication_handler,
        })
    }

    /// Start the mesh node: begin listening for segment mirrors.
    /// For Step 2, peer discovery is manual via register_peer().
    /// Step 3 will add gossip-based auto-discovery.
    pub async fn start(&self, _seed_addrs: Vec<SocketAddr>) -> Result<()> {
        // Start TCP listener for segment mirroring
        self.start_mirror_listener().await?;

        info!(node_id = %self.id, "mesh node started");
        Ok(())
    }

    /// Start listening for incoming segment mirrors via TCP (RDMA mock).
    async fn start_mirror_listener(&self) -> Result<()> {
        let listener = TcpListener::bind(self.listen_addr)
            .await
            .map_err(|e| anyhow!("failed to bind mirror listener: {}", e))?;

        info!(addr = %self.listen_addr, "mirror listener started");

        let handler = self.replication_handler.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((socket, addr)) => {
                        debug!(remote = %addr, "accepted mirror connection");
                        let handler_clone = handler.clone();
                        tokio::spawn(async move {
                            handler_clone.handle_connection(socket).await;
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to accept connection");
                    }
                }
            }
        });

        Ok(())
    }

    /// Discover peer nodes via gossip.
    /// Returns a list of NodeIds for replication targets.
    pub async fn discover_peers(&self) -> Result<Vec<NodeId>> {
        // For Step 2, return manually registered peers
        // In Step 3, integrate full gossip discovery
        let peers = self.peers.read().await;
        let peer_ids: Vec<NodeId> = peers.keys().copied().collect();

        debug!(count = peer_ids.len(), "discovered peers (manual registry)");
        Ok(peer_ids)
    }

    /// Mirror a segment to a target node.
    ///
    /// This wraps the payload in a replication frame so inbound handlers can
    /// apply MAC validation, encryption, and deduplication consistently.
    pub async fn mirror_segment(
        &self,
        segment_id: SegmentId,
        segment_data: &[u8],
        target: NodeId,
    ) -> Result<()> {
        let mut metadata = encryption::policy::EncryptionMetadata::new_unencrypted();
        metadata.ciphertext_len = Some(segment_data.len() as u32);

        let frame =
            replication::ReplicationFrame::new(segment_id, metadata, segment_data.to_vec());

        self.send_replication_frame(&frame, target).await
    }

    /// Send a complete replication frame to a target node.
    /// This is the full replication protocol that includes encryption metadata.
    pub async fn send_replication_frame(
        &self,
        frame: &replication::ReplicationFrame,
        target: NodeId,
    ) -> Result<()> {
        // Lookup target address from peer registry
        let peers = self.peers.read().await;
        let target_addr = peers
            .get(&target)
            .ok_or_else(|| anyhow!("target node {} not found in peer registry", target))?
            .clone();
        drop(peers);

        debug!(
            target_id = %target,
            target_addr = %target_addr,
            segment_id = frame.segment_id.0,
            "sending replication frame"
        );

        // Connect to target
        let mut stream = TcpStream::connect(&target_addr)
            .await
            .map_err(|e| anyhow!("failed to connect to target {}: {}", target_addr, e))?;

        // Serialize and send frame with length prefix
        let frame_bytes = frame.to_bytes()?;
        stream
            .write_all(&frame_bytes)
            .await
            .map_err(|e| anyhow!("failed to write frame: {}", e))?;

        stream
            .shutdown()
            .await
            .map_err(|e| anyhow!("failed to shutdown stream: {}", e))?;

        info!(
            target_id = %target,
            segment_id = frame.segment_id.0,
            bytes = frame_bytes.len(),
            "replication frame sent successfully"
        );

        Ok(())
    }

    /// Register a peer node with its address.
    /// Called during discovery to populate the peer registry.
    pub async fn register_peer(&self, peer_id: NodeId, addr: SocketAddr) {
        let mut peers = self.peers.write().await;
        peers.insert(peer_id, addr);
        debug!(peer_id = %peer_id, addr = %addr, "registered peer");
    }

    /// Get this node's ID.
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Get this node's zone.
    pub fn zone(&self) -> &ZoneId {
        &self.zone
    }

    /// Get this node's capabilities.
    pub fn capabilities(&self) -> &NodeCapabilities {
        &self.capabilities
    }

    #[cfg(feature = "phase4")]
    pub async fn resolve_federated(&self, id: CapsuleId) -> Result<NodeId> {
        let peers = self.discover_peers().await?;
        let target = peers.first().copied().unwrap_or(self.id);
        info!(
            capsule = %id.as_uuid(),
            target = %target,
            "resolved federated mesh target"
        );
        Ok(target)
    }

    #[cfg(feature = "phase4")]
    pub async fn federate_capsule(&self, id: CapsuleId, zone: ZoneId) -> Result<()> {
        let cluster = RaftCluster::new(RaftClusterConfig::default());
        let zone_ref = zone.to_string();
        cluster
            .replicate(&id.as_uuid().to_string(), &zone_ref)
            .await?;
        info!(
            capsule = %id.as_uuid(),
            zone = %zone_ref,
            "triggering federated capsule replication"
        );
        Ok(())
    }

    #[cfg(feature = "phase4")]
    pub async fn shard_metadata(
        &self,
        id: CapsuleId,
        shards: Vec<MetadataShard>,
        payload: &[u8],
    ) -> Result<()> {
        for shard in shards {
            let cluster = RaftCluster::for_zone(&shard.zone.to_string());
            cluster
                .store_shard(&ShardKey::new(shard.shard_id), payload)
                .await?;
            info!(
                capsule = %id.as_uuid(),
                shard = shard.shard_id,
                owner = %shard.owner,
                zone = %shard.zone,
                "stored metadata shard"
            );
        }
        Ok(())
    }
}

// Tests are in tests.rs module

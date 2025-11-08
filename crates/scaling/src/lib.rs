//! PODMS Scaling Module - Metro-Sync Replication and Mesh Networking
//!
//! This module implements the core distribution capabilities for PODMS Step 2:
//! - Mesh networking with gossip-based peer discovery
//! - RDMA mocks for zero-copy data transport
//! - Metro-sync replication for zero-RPO policies
//! - Scaling agents for autonomous telemetry-driven migrations

use anyhow::{anyhow, Result};
use common::podms::{NodeId, ZoneId};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

pub mod agent;
#[cfg(test)]
mod tests;

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
    Standard,  // <10ms metro latency
    Premium,   // <2ms with RDMA
    Edge,      // >50ms edge sites
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

/// Mesh node for PODMS distribution.
/// Handles peer discovery via gossip and provides zero-copy segment mirroring.
pub struct MeshNode {
    id: NodeId,
    zone: ZoneId,
    capabilities: NodeCapabilities,
    /// Peer registry: NodeId -> SocketAddr
    /// For Step 2, peers are manually registered
    /// Step 3 will add gossip-based auto-discovery
    peers: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    /// Local listen address for mirroring
    listen_addr: SocketAddr,
}

impl MeshNode {
    /// Create a new mesh node in the specified zone.
    /// Initializes gossip discovery but doesn't join until `start()` is called.
    pub async fn new(zone: ZoneId, listen_addr: SocketAddr) -> Result<Self> {
        let id = NodeId::new();
        let capabilities = NodeCapabilities::default();

        info!(
            node_id = %id,
            zone = %zone,
            listen_addr = %listen_addr,
            "creating mesh node"
        );

        Ok(Self {
            id,
            zone,
            capabilities,
            peers: Arc::new(RwLock::new(HashMap::new())),
            listen_addr,
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

        let peers = self.peers.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((socket, addr)) => {
                        debug!(remote = %addr, "accepted mirror connection");
                        tokio::spawn(Self::handle_mirror_connection(socket, peers.clone()));
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to accept connection");
                    }
                }
            }
        });

        Ok(())
    }

    /// Handle an incoming mirror connection (segment replication).
    async fn handle_mirror_connection(
        mut socket: TcpStream,
        _peers: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    ) {
        // TODO: Implement segment receive logic
        // For now, just read and discard data
        let mut buf = vec![0u8; 65536];
        loop {
            match socket.try_read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    debug!(bytes = n, "received mirror data");
                    // TODO: Persist segment via NvramLog
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!(error = %e, "mirror read error");
                    break;
                }
            }
        }
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

    /// Mirror a segment to a target node using RDMA mock (TCP for POC).
    /// In production, this would use RDMA verbs for zero-copy transfer.
    pub async fn mirror_segment(&self, segment_data: &[u8], target: NodeId) -> Result<()> {
        // Lookup target address from peer registry
        let peers = self.peers.read().await;
        let target_addr = peers
            .get(&target)
            .ok_or_else(|| anyhow!("target node {} not found in peer registry", target))?;

        debug!(
            target_id = %target,
            target_addr = %target_addr,
            bytes = segment_data.len(),
            "mirroring segment"
        );

        // RDMA mock: Use TCP for simulation
        // In production: Use rdma-sys or similar for zero-copy RDMA writes
        let mut stream = TcpStream::connect(target_addr)
            .await
            .map_err(|e| anyhow!("failed to connect to target {}: {}", target_addr, e))?;

        stream
            .write_all(segment_data)
            .await
            .map_err(|e| anyhow!("failed to write segment: {}", e))?;

        stream
            .shutdown()
            .await
            .map_err(|e| anyhow!("failed to shutdown stream: {}", e))?;

        info!(
            target_id = %target,
            bytes = segment_data.len(),
            "segment mirrored successfully"
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
}

// Tests are in tests.rs module

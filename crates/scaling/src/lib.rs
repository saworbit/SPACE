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
use common::{podms::Telemetry, CapsuleId, Policy};
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::Mutex as ThreadHandleMutex;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
#[cfg(target_os = "linux")]
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::transport::ConnectionManager;

#[cfg(feature = "phase4")]
use raft_rs::{RaftCluster, RaftClusterConfig, ShardKey};

pub mod agent;
pub mod batch_queue;
pub mod compiler;
pub mod replication;
pub mod swarm_ops;
#[cfg(test)]
mod tests;
mod transport;

// Re-export key compiler types for external use
pub use compiler::{
    EvacuationUrgency, MeshState, NodeInfo, PolicyCompiler, ReplicationStrategy, ScalingAction,
};

// Re-export replication types for external use
pub use replication::{ContentStore, ReplicationFrame, ReplicationHandler};

// Re-export batch queue types for external use
pub use batch_queue::{BatchItem, BatchQueue, BatchQueueSender, QueueStats};

// Re-export SwarmOps for PODMS migrations
pub use swarm_ops::SwarmOps;

/// Enforce view-scope scaling actions (federation/sharding) before projection.
#[cfg(feature = "phase4")]
pub async fn enforce_view_policy<C, F>(
    mesh: &MeshNode<C>,
    capsule_id: CapsuleId,
    policy: &Policy,
    view_name: &str,
    shard_serializer: F,
) -> Result<()>
where
    C: ContentStore + 'static,
    F: Fn(CapsuleId) -> Result<Vec<u8>>,
{
    let telemetry = Telemetry::ViewProjection {
        id: capsule_id,
        view: view_name.into(),
    };

    let mesh_state = MeshState::empty(mesh.zone().clone());
    let actions = compiler::compile_scaling(policy, &telemetry, &mesh_state);

    for action in actions {
        match action {
            ScalingAction::Federate { capsule_id, zone } => {
                mesh.federate_capsule(capsule_id, zone).await?;
            }
            ScalingAction::ShardEC {
                capsule_id, zones, ..
            } => {
                if zones.is_empty() {
                    continue;
                }

                let payload = shard_serializer(capsule_id)?;
                let shard_keys = capsule_id.shard_keys(zones.len());
                let shards: Vec<MetadataShard> = zones
                    .into_iter()
                    .zip(shard_keys.into_iter())
                    .map(|(zone, shard_id)| MetadataShard {
                        shard_id,
                        owner: mesh.id(),
                        zone,
                    })
                    .collect();

                mesh.shard_metadata(capsule_id, shards, &payload).await?;
            }
            _ => {
                // Background agents handle other actions (replication/migration).
            }
        }
    }

    Ok(())
}

#[async_trait::async_trait]
trait DataTransport: Send + Sync {
    async fn send_frame(
        &self,
        target: NodeId,
        target_addr: SocketAddr,
        frame: Vec<u8>,
    ) -> Result<()>;
}

/// Standard TCP transport used for cross-platform builds with persistent streams.
#[cfg_attr(target_os = "linux", allow(dead_code))]
struct TcpTransport {
    connections: ConnectionManager,
}

impl TcpTransport {
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    fn new() -> Self {
        Self {
            connections: ConnectionManager::new(),
        }
    }
}

#[async_trait::async_trait]
impl DataTransport for TcpTransport {
    async fn send_frame(
        &self,
        target: NodeId,
        target_addr: SocketAddr,
        frame: Vec<u8>,
    ) -> Result<()> {
        let mut attempts = 0;

        loop {
            let writer_guard = self.connections.get_writer(target, target_addr).await?;
            let mut writer = writer_guard.lock().await;

            match writer.write_all(&frame).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    attempts += 1;
                    if attempts > 1 {
                        return Err(anyhow!("failed to send frame after retry: {}", e));
                    }

                    drop(writer);
                    self.connections.invalidate(target).await;
                }
            }
        }
    }
}

/// Linux-native io_uring transport using a dedicated actor thread with persistent connections.
#[cfg(target_os = "linux")]
struct IoUringTransport {
    command_tx: mpsc::UnboundedSender<TransportCommand>,
    ring_thread: ThreadHandleMutex<Option<std::thread::JoinHandle<()>>>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
enum TransportCommand {
    SendFrame {
        target: NodeId,
        addr: SocketAddr,
        data: Vec<u8>,
        resp: Option<oneshot::Sender<Result<()>>>,
    },
    Disconnect {
        target: NodeId,
    },
    Shutdown,
}

#[cfg(target_os = "linux")]
struct SendWork {
    data: Vec<u8>,
    resp: Option<oneshot::Sender<Result<()>>>,
}

#[cfg(target_os = "linux")]
struct ConnectionEntry {
    tx: mpsc::Sender<SendWork>,
    addr: SocketAddr,
}

#[cfg(target_os = "linux")]
struct ActorState {
    connections: HashMap<NodeId, ConnectionEntry>,
}

#[cfg(target_os = "linux")]
impl ActorState {
    fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    async fn ensure_connection(
        &mut self,
        target: NodeId,
        addr: SocketAddr,
    ) -> Result<mpsc::Sender<SendWork>> {
        if let Some(entry) = self.connections.get(&target) {
            if entry.addr == addr {
                return Ok(entry.tx.clone());
            }

            self.connections.remove(&target);
        }

        let (tx, mut rx) = mpsc::channel::<SendWork>(128);
        let stream = tokio_uring::net::TcpStream::connect(addr)
            .await
            .map_err(|e| anyhow!("io_uring connect to {} failed: {}", addr, e))?;

        tokio_uring::spawn(async move {
            let mut stream = stream;
            while let Some(work) = rx.recv().await {
                let (res, _buf) = stream.write_all(work.data).await;
                if let Some(resp) = work.resp {
                    let _ = resp.send(
                        res.map(|_| ())
                            .map_err(|e| anyhow!("io_uring write failed: {}", e)),
                    );
                }

                if res.is_err() {
                    break;
                }
            }
        });

        self.connections.insert(
            target,
            ConnectionEntry {
                tx: tx.clone(),
                addr,
            },
        );

        Ok(tx)
    }

    fn drop_connection(&mut self, target: &NodeId) {
        self.connections.remove(target);
    }
}

#[cfg(target_os = "linux")]
impl IoUringTransport {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = std::thread::spawn(move || {
            tokio_uring::start(async move {
                run_uring_actor(rx).await;
            });
        });

        Self {
            command_tx: tx,
            ring_thread: ThreadHandleMutex::new(Some(handle)),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for IoUringTransport {
    fn drop(&mut self) {
        let _ = self.command_tx.send(TransportCommand::Shutdown);
        if let Ok(mut handle_opt) = self.ring_thread.lock() {
            if let Some(handle) = handle_opt.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn run_uring_actor(mut rx: mpsc::UnboundedReceiver<TransportCommand>) {
    let mut state = ActorState::new();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TransportCommand::SendFrame {
                target,
                addr,
                data,
                resp,
            } => {
                if let Err(e) = handle_send(&mut state, target, addr, data, resp).await {
                    tracing::error!(target = %target, addr = %addr, error = %e, "io_uring send failed");
                }
            }
            TransportCommand::Disconnect { target } => {
                state.drop_connection(&target);
            }
            TransportCommand::Shutdown => break,
        }
    }
}

#[cfg(target_os = "linux")]
async fn handle_send(
    state: &mut ActorState,
    target: NodeId,
    addr: SocketAddr,
    data: Vec<u8>,
    resp: Option<oneshot::Sender<Result<()>>>,
) -> Result<()> {
    let sender = match state.ensure_connection(target, addr).await {
        Ok(sender) => sender,
        Err(err) => {
            if let Some(resp) = resp {
                let _ = resp.send(Err(anyhow!(err.to_string())));
            }
            return Err(err);
        }
    };

    match sender.send(SendWork { data, resp }).await {
        Ok(_) => Ok(()),
        Err(mpsc::error::SendError(work)) => {
            state.drop_connection(&target);
            if let Some(resp) = work.resp {
                let _ = resp.send(Err(anyhow!(
                    "io_uring connection task closed for target {}",
                    target
                )));
            }
            Err(anyhow!(
                "io_uring connection task closed for target {}",
                target
            ))
        }
    }
}

#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl DataTransport for IoUringTransport {
    async fn send_frame(
        &self,
        target: NodeId,
        target_addr: SocketAddr,
        frame: Vec<u8>,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(TransportCommand::SendFrame {
                target,
                addr: target_addr,
                data: frame,
                resp: Some(resp_tx),
            })
            .map_err(|_| anyhow!("io_uring runtime closed"))?;

        match resp_rx.await {
            Ok(res) => res,
            Err(_) => Err(anyhow!(
                "io_uring actor shut down before send completion for target {}",
                target
            )),
        }
    }
}

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
    /// Transport abstraction (io_uring on Linux, TCP elsewhere)
    transport: Arc<dyn DataTransport>,
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

        #[cfg(target_os = "linux")]
        let transport: Arc<dyn DataTransport> = {
            info!("initializing io_uring actor transport with persistent connections");
            Arc::new(IoUringTransport::new())
        };

        #[cfg(not(target_os = "linux"))]
        let transport: Arc<dyn DataTransport> = {
            info!("initializing TCP transport (standard copy path)");
            Arc::new(TcpTransport::new())
        };

        Ok(Self {
            id,
            zone,
            capabilities,
            peers: Arc::new(RwLock::new(HashMap::new())),
            listen_addr,
            replication_handler,
            transport,
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

        let frame = replication::ReplicationFrame::new(segment_id, metadata, segment_data.to_vec());

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
        let target_addr = *peers
            .get(&target)
            .ok_or_else(|| anyhow!("target node {} not found in peer registry", target))?;
        drop(peers);

        debug!(
            target_id = %target,
            target_addr = %target_addr,
            segment_id = frame.segment_id.0,
            "preparing replication frame"
        );

        let frame_bytes = frame.to_bytes()?;
        let frame_len = frame_bytes.len();

        self.transport
            .send_frame(target, target_addr, frame_bytes)
            .await?;

        info!(
            target_id = %target,
            segment_id = frame.segment_id.0,
            bytes = frame_len,
            "replication frame enqueued for delivery"
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

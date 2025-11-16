//! PODMS Orchestrator - Multi-Node Coordination Layer
//!
//! This crate provides the orchestration layer that wires together all multi-node
//! components into a cohesive distributed system:
//!
//! - **Gossip Layer**: Epidemic state propagation for metadata and events
//! - **Mesh Networking**: P2P connectivity and data replication
//! - **Policy Compiler**: Intelligent decision-making based on PODMS policies
//! - **Scaling Agent**: Autonomous execution of scaling actions
//! - **Telemetry Bus**: Event-driven coordination across components
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    PODMS Orchestrator                       │
//! ├─────────────────────────────────────────────────────────────┤
//! │                                                             │
//! │  ┌──────────────┐      ┌──────────────┐                   │
//! │  │ Gossip Layer │ ───► │ Mesh Network │                   │
//! │  │  (libp2p)    │      │  Discovery   │                   │
//! │  └──────────────┘      └──────────────┘                   │
//! │         │                      │                            │
//! │         ▼                      ▼                            │
//! │  ┌──────────────────────────────────┐                      │
//! │  │      Telemetry Bus (mpsc)        │                      │
//! │  └──────────────────────────────────┘                      │
//! │         │                      │                            │
//! │         ▼                      ▼                            │
//! │  ┌──────────────┐      ┌──────────────┐                   │
//! │  │Policy Compiler│      │ Scaling Agent│                   │
//! │  │  (swarm AI)  │ ───► │ (autonomous)  │                   │
//! │  └──────────────┘      └──────────────┘                   │
//! │                               │                             │
//! │                               ▼                             │
//! │                     ┌──────────────────┐                   │
//! │                     │ Replication      │                   │
//! │                     │ Migration        │                   │
//! │                     │ Transformation   │                   │
//! │                     └──────────────────┘                   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use podms_orchestrator::{Orchestrator, OrchestratorConfig};
//! use common::Policy;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Load configuration from YAML or environment
//!     let config = OrchestratorConfig::from_yaml_file("/etc/space/orchestrator.yml")?;
//!
//!     // Create orchestrator with required dependencies
//!     // (content_store, catalog, nvram_log, key_manager)
//!     let mut orchestrator = Orchestrator::new(
//!         config,
//!         content_store,
//!         catalog,
//!         nvram_log,
//!         key_manager,
//!     ).await?;
//!
//!     // Start orchestrator (launches all subsystems)
//!     orchestrator.start().await?;
//!
//!     // Orchestrator now running autonomously...
//!     orchestrator.wait().await?;
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use common::podms::{NodeId, Telemetry, ZoneId};
use common::traits::CapsuleCatalog;
use encryption::keymanager::KeyManager;
use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, GossipHandler};
use nvram_sim::NvramLog;
use scaling::agent::ScalingAgent;
use scaling::{ContentStore, MeshNode};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

mod config;
mod runtime;

pub use config::OrchestratorConfig;
pub use runtime::OrchestratorRuntime;

/// The main orchestrator that coordinates all multi-node subsystems.
///
/// This is the entry point for deploying SPACE in multi-node mode. It:
/// - Initializes the gossip layer for state propagation
/// - Creates the mesh node for P2P replication
/// - Wires the scaling agent for autonomous operations
/// - Connects telemetry channels for event-driven coordination
///
/// # Lifecycle
///
/// 1. **Initialization**: `Orchestrator::new(config)`
/// 2. **Start**: `orchestrator.start()` - Launches all subsystems
/// 3. **Runtime**: Autonomous operation driven by telemetry events
/// 4. **Shutdown**: Drop or explicit `shutdown()` call
pub struct Orchestrator<C: ContentStore> {
    /// Orchestrator configuration
    config: OrchestratorConfig,

    /// Gossip handler for state propagation
    gossip: Arc<GossipImpl>,

    /// Mesh node for P2P replication
    mesh_node: Arc<MeshNode<C>>,

    /// Scaling agent for autonomous actions
    scaling_agent: ScalingAgent<C>,

    /// Telemetry channel sender
    telemetry_tx: mpsc::UnboundedSender<Telemetry>,

    /// Telemetry channel receiver (owned by agent)
    telemetry_rx: Option<mpsc::UnboundedReceiver<Telemetry>>,

    /// Background task handles
    tasks: JoinSet<Result<()>>,
}

impl<C: ContentStore + 'static> Orchestrator<C> {
    /// Create a new orchestrator with the specified configuration.
    ///
    /// This initializes all subsystems but does not start them yet.
    /// Call `start()` to begin autonomous operation.
    ///
    /// # Arguments
    ///
    /// * `config` - Orchestrator configuration (node ID, address, policy, etc.)
    /// * `content_store` - Content store for deduplication (typically CapsuleRegistry)
    /// * `catalog` - Capsule catalog for migration operations
    /// * `nvram_log` - NVRAM log for segment persistence
    /// * `key_manager` - Key manager for encryption operations
    pub async fn new(
        config: OrchestratorConfig,
        content_store: Arc<RwLock<C>>,
        catalog: Arc<dyn CapsuleCatalog + Send + Sync>,
        nvram_log: Arc<RwLock<NvramLog>>,
        key_manager: Arc<RwLock<KeyManager>>,
    ) -> Result<Self> {
        info!(
            node_id = %config.node_id,
            listen_addr = %config.listen_addr,
            "initializing PODMS orchestrator"
        );

        // 1. Initialize gossip layer
        let gossip_config = GossipConfig {
            fanout: config.gossip_fanout,
            heartbeat_interval_ms: config.heartbeat_interval_ms,
            message_ttl: config.message_ttl,
            max_message_size: config.max_message_size,
            enable_compression: true,
            enable_encryption: true,
            signing_key: config.signing_key.clone(),
        };

        let gossip = GossipImpl::new(gossip_config)
            .await
            .context("failed to initialize gossip layer")?;

        info!(peer_id = %gossip.peer_id(), "gossip layer initialized");

        // 2. Initialize mesh node
        let zone = ZoneId::Metro {
            name: config.zone_name.clone(),
        };

        let mesh_node = MeshNode::new(
            zone,
            config.listen_addr,
            content_store,
            nvram_log.clone(),
            key_manager.clone(),
        )
        .await
        .context("failed to initialize mesh node")?;

        info!(
            node_id = %mesh_node.id(),
            zone = %mesh_node.zone(),
            "mesh node initialized"
        );

        // Wrap mesh_node in Arc for shared ownership
        let mesh_node_arc = Arc::new(mesh_node);

        // 3. Create telemetry channel
        let (telemetry_tx, telemetry_rx) = mpsc::unbounded_channel();

        // 4. Initialize scaling agent with runtime dependencies
        let scaling_agent = ScalingAgent::with_runtime(
            mesh_node_arc.clone(),
            config.default_policy.clone(),
            catalog,
            nvram_log,
            key_manager,
        );

        info!("scaling agent initialized");

        Ok(Self {
            config,
            gossip: Arc::new(gossip),
            mesh_node: mesh_node_arc,
            scaling_agent,
            telemetry_tx,
            telemetry_rx: Some(telemetry_rx),
            tasks: JoinSet::new(),
        })
    }

    /// Start the orchestrator and all subsystems.
    ///
    /// This launches:
    /// - Mesh node listener (for incoming replication)
    /// - Scaling agent (consumes telemetry events)
    /// - Gossip event bridge (forwards gossip to telemetry)
    /// - Peer discovery (via seed peers)
    ///
    /// After calling this, the orchestrator operates autonomously.
    pub async fn start(&mut self) -> Result<()> {
        info!(node_id = %self.config.node_id, "starting PODMS orchestrator");

        // 1. Start mesh node (TCP listener for replication)
        let seed_addrs = self.config.seed_peers.clone();
        self.mesh_node
            .start(seed_addrs.clone())
            .await
            .context("failed to start mesh node")?;

        info!("mesh node started and listening");

        // 2. Register seed peers
        for addr in seed_addrs {
            // In a real implementation, we would:
            // - Connect to seed peer
            // - Exchange node IDs via handshake
            // - Register in mesh node
            //
            // For now, we just log that we would discover them
            info!(seed_addr = %addr, "discovering seed peer");
        }

        // 3. Subscribe to gossip topics and bridge to telemetry
        let gossip_clone = self.gossip.clone();
        let telemetry_tx_clone = self.telemetry_tx.clone();

        self.tasks.spawn(async move {
            Self::gossip_to_telemetry_bridge(gossip_clone, telemetry_tx_clone).await
        });

        info!("gossip-to-telemetry bridge started");

        // 4. Start scaling agent (consumes telemetry)
        let telemetry_rx = self
            .telemetry_rx
            .take()
            .context("telemetry receiver already taken")?;

        let scaling_agent_moved = std::mem::replace(
            &mut self.scaling_agent,
            ScalingAgent::new(self.mesh_node.clone()),
        );

        self.tasks.spawn(async move {
            scaling_agent_moved
                .run(telemetry_rx)
                .await
                .context("scaling agent failed")
        });

        info!("scaling agent started");

        info!(node_id = %self.config.node_id, "PODMS orchestrator fully operational");

        Ok(())
    }

    /// Bridge gossip events to telemetry channel.
    ///
    /// This subscribes to key gossip topics and translates gossip messages
    /// into telemetry events that the scaling agent can consume.
    async fn gossip_to_telemetry_bridge(
        gossip: Arc<GossipImpl>,
        telemetry_tx: mpsc::UnboundedSender<Telemetry>,
    ) -> Result<()> {
        // Subscribe to key topics
        let mut updates_rx = gossip
            .subscribe("updates")
            .await
            .context("failed to subscribe to updates topic")?;

        info!("subscribed to gossip topics, bridging to telemetry");

        // Process gossip messages and forward as telemetry
        while let Some(msg) = updates_rx.recv().await {
            // Translate gossip messages to telemetry events
            match msg {
                mesh_core::GossipMessage::Heartbeat {
                    peer_id: _,
                    storage_usage,
                    timestamp: _,
                } => {
                    // Could emit capacity telemetry based on storage usage
                    if storage_usage > 1_000_000_000_000 {
                        // >1TB usage
                        // Note: We use a placeholder node_id here because peer_id is a libp2p PeerId string
                        // In a production system, we'd maintain a mapping from PeerId -> NodeId
                        let node_id = NodeId::new(); // Placeholder
                        let event = Telemetry::CapacityThreshold {
                            node_id,
                            used_bytes: storage_usage,
                            total_bytes: storage_usage + 100_000_000, // Mock total
                            threshold_pct: 0.9,
                        };

                        if telemetry_tx.send(event).is_err() {
                            error!("telemetry channel closed, shutting down bridge");
                            break;
                        }
                    }
                }
                mesh_core::GossipMessage::DataMigration { path, .. } => {
                    info!(path = %path, "received migration gossip message");
                    // Could emit migration telemetry
                }
                mesh_core::GossipMessage::SecurityAlert {
                    severity, threat, ..
                } => {
                    warn!(severity = %severity, threat = %threat, "security alert received");
                    // Could trigger scaling actions or policy updates
                }
                _ => {
                    // Other gossip messages (log but don't generate telemetry)
                }
            }
        }

        info!("gossip-to-telemetry bridge terminated");
        Ok(())
    }

    /// Get a handle to the gossip layer for external use.
    pub fn gossip(&self) -> Arc<GossipImpl> {
        self.gossip.clone()
    }

    /// Get a handle to the mesh node for external use.
    pub fn mesh_node(&self) -> Arc<MeshNode<C>> {
        self.mesh_node.clone()
    }

    /// Get the telemetry sender for emitting events.
    pub fn telemetry_sender(&self) -> mpsc::UnboundedSender<Telemetry> {
        self.telemetry_tx.clone()
    }

    /// Get the node ID.
    pub fn node_id(&self) -> NodeId {
        self.mesh_node.id()
    }

    /// Shutdown the orchestrator gracefully.
    ///
    /// This stops all background tasks and closes channels.
    pub async fn shutdown(mut self) -> Result<()> {
        info!(node_id = %self.config.node_id, "shutting down PODMS orchestrator");

        // Abort all background tasks
        self.tasks.shutdown().await;

        info!("orchestrator shutdown complete");
        Ok(())
    }

    /// Wait for all background tasks to complete (runs forever unless shutdown).
    pub async fn wait(&mut self) -> Result<()> {
        while let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(Ok(())) => {
                    info!("background task completed successfully");
                }
                Ok(Err(e)) => {
                    error!(error = %e, "background task failed");
                    return Err(e);
                }
                Err(e) => {
                    error!(error = %e, "background task panicked");
                    return Err(anyhow::anyhow!("task panic: {}", e));
                }
            }
        }

        info!("all background tasks completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Tests require a concrete ContentStore implementation
    // They will be added once we integrate with CapsuleRegistry
}

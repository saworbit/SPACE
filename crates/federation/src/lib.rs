//! Phase 4 Federation Plane ("The Bridge").
//!
//! The production implementation subscribes to Raft/audit logs and performs
//! inter-zone replication asynchronously. In this repository we simulate that
//! behavior by copying capsule metadata + referenced segments into zone-scoped
//! registries and NVRAM logs.

pub mod bridge;
pub mod engine;
pub mod queue;
pub mod registry;
pub mod rpc;
pub mod scheduler;
pub mod server;
pub mod state;
pub mod storage;
pub mod transport;
pub mod wan;
pub mod zones;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZoneConfig {
    pub name: String,
    #[serde(alias = "url")]
    pub endpoint: String,
    #[serde(alias = "secret")]
    pub secret_key: String,
}

pub use bridge::{Bridge, FederationBridge, FederationResult};
pub use engine::{RaftEngine, RaftEngineConfig};
pub use registry::{
    build_create_volume_cmd, build_create_volume_cmd_with_source, build_delete_volume_cmd,
    build_move_replica_cmd, build_register_node_cmd, ClusterState, NodeMetadata, NodeStatus,
    PendingAllocation, PendingAllocations, Registry, VolumeMetadata,
};
pub use scheduler::{PlacementRequirements, Scheduler};
pub use server::FederationServiceImpl;
pub use server::{serve, serve_from_paths};
pub use storage::SledStorage;
pub use transport::{start_raft_server, PeerRegistry, RaftServiceImpl, RaftTransportClient};

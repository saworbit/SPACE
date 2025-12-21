//! Phase 4 Federation Plane ("The Bridge").
//!
//! The production implementation subscribes to Raft/audit logs and performs
//! inter-zone replication asynchronously. In this repository we simulate that
//! behavior by copying capsule metadata + referenced segments into zone-scoped
//! registries and NVRAM logs.

pub mod bridge;
pub mod queue;
pub mod rpc;
pub mod server;
pub mod state;
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
pub use server::FederationServiceImpl;
pub use server::{serve, serve_from_paths};

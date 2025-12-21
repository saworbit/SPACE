//! Phase 4 Federation Plane ("The Bridge").
//!
//! The production implementation subscribes to Raft/audit logs and performs
//! inter-zone replication asynchronously. In this repository we simulate that
//! behavior by copying capsule metadata + referenced segments into zone-scoped
//! registries and NVRAM logs.

#![cfg(feature = "phase4")]

pub mod bridge;
pub mod queue;
pub mod rpc;
pub mod server;
pub mod state;
pub mod wan;
pub mod zones;

pub use bridge::{Bridge, FederationResult};
pub use server::FederationServiceImpl;
pub use server::{serve, serve_from_paths};

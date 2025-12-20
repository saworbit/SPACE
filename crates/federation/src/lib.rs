//! Phase 4 Federation Plane ("The Bridge").
//!
//! The production implementation subscribes to Raft/audit logs and performs
//! inter-zone replication asynchronously. In this repository we simulate that
//! behavior by copying capsule metadata + referenced segments into zone-scoped
//! registries and NVRAM logs.

#![cfg(feature = "phase4")]

pub mod bridge;

pub use bridge::{FederationBridge, FederationResult};

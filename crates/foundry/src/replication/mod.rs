//! Chain Replication for High Availability.
//!
//! This module implements synchronous chain replication to ensure zero RPO
//! (Recovery Point Objective). Every write to a primary volume is replicated
//! to a secondary node before acknowledging success to the client.
//!
//! ## Architecture
//!
//! - **Primary Node**: Writes locally, pushes to replica, waits for ack, then acks client
//! - **Replica Node**: Receives stream, writes locally, acks primary
//! - **Transport**: Raw TCP with length-delimited framing for maximum throughput
//!
//! ## Usage
//!
//! ### Primary Node (with replication)
//!
//! ```no_run
//! use foundry::{Foundry, BackendType, VolumeId};
//! use foundry::replication::{ReplicationClient, ReplicatedBackend};
//! use std::sync::Arc;
//!
//! # async fn example() -> foundry::error::Result<()> {
//! let foundry = Foundry::new();
//! let volume_id = VolumeId::new();
//!
//! // Create local volume
//! let local = foundry.create_volume(volume_id, 10 * 1024 * 1024, None).await?;
//!
//! // Connect to replica
//! let replicator = ReplicationClient::connect(
//!     "127.0.0.1:4421",
//!     volume_id.to_string()
//! ).await?;
//!
//! // Wrap with replication
//! let replicated = Arc::new(ReplicatedBackend::new(local, replicator));
//!
//! // Writes are now replicated synchronously
//! replicated.write_at(0, bytes::Bytes::from(vec![0x42; 4096])).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Replica Node (server)
//!
//! ```no_run
//! use foundry::Foundry;
//! use foundry::replication::start_replication_server;
//! use std::sync::Arc;
//!
//! # async fn example() -> foundry::error::Result<()> {
//! let foundry = Arc::new(Foundry::new());
//!
//! // Start replication server on port 4421
//! start_replication_server(foundry, 4421).await?;
//! # Ok(())
//! # }
//! ```

pub mod actor;
pub mod protocol;
pub mod replicated;
pub mod server;

#[cfg(test)]
mod tests;

pub use actor::ReplicationClient;
pub use replicated::ReplicatedBackend;
pub use server::start_replication_server;

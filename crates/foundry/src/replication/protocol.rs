//! Replication protocol definitions for chain replication.
//!
//! This module defines the wire protocol for synchronous replication between
//! primary and replica nodes.

use serde::{Deserialize, Serialize};

/// Messages sent from primary to replica.
#[derive(Debug, Serialize, Deserialize)]
pub enum ReplicationMessage {
    /// Initial handshake to identify volume.
    Handshake {
        volume_id: String,
    },
    /// A write operation to be applied.
    Write {
        offset: u64,
        data: Vec<u8>,
    },
    /// Acknowledge a specific operation was persisted.
    Ack,
}

/// Responses sent from replica to primary.
#[derive(Debug, Serialize, Deserialize)]
pub enum ReplicationResponse {
    /// Operation successful.
    Ok,
    /// Operation failed with error message.
    Error(String),
}

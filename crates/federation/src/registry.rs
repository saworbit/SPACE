//! Phase 9.3: The Global Registry (State Machine)
//!
//! Deterministic state machine that applies commands from the Raft log
//! to maintain cluster topology (nodes, volumes, replica placement).

use crate::rpc;
use anyhow::{anyhow, Result};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

/// Represents the topology of the entire cluster.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    pub nodes: HashMap<u64, NodeMetadata>,
    pub volumes: HashMap<String, VolumeMetadata>,
    pub last_applied_index: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    pub id: u64,
    pub address: String,
    pub capacity: u64,
    pub status: NodeStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeStatus {
    Active,
    Draining,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMetadata {
    pub id: String,
    pub size: u64,
    /// The Raft Group ID responsible for this volume (Data Plane replication)
    /// In simple federation, this list defines the Chain: [Primary, Replica1, Replica2]
    pub replicas: Vec<u64>,
}

/// The State Machine wrapper ensuring thread safety.
pub struct Registry {
    state: Arc<RwLock<ClusterState>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ClusterState::default())),
        }
    }

    /// Returns a snapshot of the current state (for Readers)
    pub fn get_state(&self) -> ClusterState {
        self.state.read().unwrap().clone()
    }

    /// Applies a committed Raft entry to the state machine.
    /// This MUST be deterministic.
    pub fn apply(&self, index: u64, data: &[u8]) -> Result<()> {
        let cmd =
            rpc::Command::decode(data).map_err(|e| anyhow!("Failed to decode command: {}", e))?;

        let mut state = self.state.write().unwrap();

        // Idempotency check (simple version)
        if index <= state.last_applied_index {
            return Ok(()); // Already applied
        }

        match cmd.payload {
            Some(rpc::command::Payload::RegisterNode(req)) => {
                info!("Apply: Register Node {} at {}", req.node_id, req.address);
                state.nodes.insert(
                    req.node_id,
                    NodeMetadata {
                        id: req.node_id,
                        address: req.address,
                        capacity: req.capacity_bytes,
                        status: NodeStatus::Active,
                    },
                );
            }
            Some(rpc::command::Payload::CreateVolume(req)) => {
                info!(
                    "Apply: Create Volume {} ({} GB)",
                    req.volume_id,
                    req.size_bytes / 1024 / 1024 / 1024
                );
                // Simple placement strategy: First N available nodes
                // Real implementation would use the LayoutEngine here.
                let replicas: Vec<u64> = state
                    .nodes
                    .keys()
                    .take(req.replication_factor as usize)
                    .cloned()
                    .collect();

                if replicas.len() < req.replication_factor as usize {
                    warn!("Not enough nodes for requested replication factor");
                }

                state.volumes.insert(
                    req.volume_id.clone(),
                    VolumeMetadata {
                        id: req.volume_id,
                        size: req.size_bytes,
                        replicas,
                    },
                );
            }
            Some(rpc::command::Payload::DeleteVolume(req)) => {
                info!("Apply: Delete Volume {}", req.volume_id);
                state.volumes.remove(&req.volume_id);
            }
            Some(rpc::command::Payload::MoveReplica(req)) => {
                if let Some(vol) = state.volumes.get_mut(&req.volume_id) {
                    if let Some(pos) = vol.replicas.iter().position(|&x| x == req.from_node) {
                        vol.replicas[pos] = req.to_node;
                        info!(
                            "Apply: Moved replica for {} from {} to {}",
                            req.volume_id, req.from_node, req.to_node
                        );
                    }
                }
            }
            None => warn!("Empty command payload received"),
        }

        state.last_applied_index = index;
        Ok(())
    }

    /// Serializes the entire state for Raft Snapshotting.
    pub fn take_snapshot(&self) -> Result<Vec<u8>> {
        let state = self.state.read().unwrap();
        // Use bincode for internal state snapshotting (faster/smaller than protobuf for full dumps)
        bincode::serialize(&*state).map_err(|e| anyhow!(e))
    }

    /// Restores state from a snapshot.
    pub fn restore_snapshot(&self, data: &[u8]) -> Result<()> {
        let new_state: ClusterState = bincode::deserialize(data)?;
        let mut state = self.state.write().unwrap();
        *state = new_state;
        info!(
            "Restored Registry State. Index: {}",
            state.last_applied_index
        );
        Ok(())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

// Helper functions to construct commands

/// Build a CreateVolume command
pub fn build_create_volume_cmd(id: &str, size: u64, replication_factor: u32) -> Vec<u8> {
    use prost::Message;

    let cmd = rpc::Command {
        payload: Some(rpc::command::Payload::CreateVolume(rpc::CreateVolume {
            volume_id: id.to_string(),
            size_bytes: size,
            replication_factor,
        })),
    };
    cmd.encode_to_vec()
}

/// Build a RegisterNode command
pub fn build_register_node_cmd(id: u64, address: &str, capacity: u64) -> Vec<u8> {
    use prost::Message;

    let cmd = rpc::Command {
        payload: Some(rpc::command::Payload::RegisterNode(rpc::RegisterNode {
            node_id: id,
            address: address.to_string(),
            capacity_bytes: capacity,
        })),
    };
    cmd.encode_to_vec()
}

/// Build a DeleteVolume command
pub fn build_delete_volume_cmd(id: &str) -> Vec<u8> {
    use prost::Message;

    let cmd = rpc::Command {
        payload: Some(rpc::command::Payload::DeleteVolume(rpc::DeleteVolume {
            volume_id: id.to_string(),
        })),
    };
    cmd.encode_to_vec()
}

/// Build a MoveReplica command
pub fn build_move_replica_cmd(volume_id: &str, from_node: u64, to_node: u64) -> Vec<u8> {
    use prost::Message;

    let cmd = rpc::Command {
        payload: Some(rpc::command::Payload::MoveReplica(rpc::MoveReplica {
            volume_id: volume_id.to_string(),
            from_node,
            to_node,
        })),
    };
    cmd.encode_to_vec()
}

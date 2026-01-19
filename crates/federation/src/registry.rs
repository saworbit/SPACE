//! Phase 9.3: The Global Registry (State Machine)
//!
//! Deterministic state machine that applies commands from the Raft log
//! to maintain cluster topology (nodes, volumes, replica placement).

use crate::rpc;
use anyhow::{anyhow, Result};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

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
    /// Phase 9.6: Optional source snapshot to hydrate from
    pub source_capsule_id: Option<String>,
}

/// Tracks a pending resource allocation that has been proposed but not yet committed.
///
/// This prevents the "Smart Leader" double-spend race condition where two concurrent
/// proposals could both see the same available capacity and over-provision a node.
#[derive(Debug, Clone)]
pub struct PendingAllocation {
    /// The volume ID being created
    pub volume_id: String,
    /// Size being allocated on each node
    pub size_bytes: u64,
    /// Node IDs that have pending allocations for this volume
    pub node_ids: Vec<u64>,
    /// When this allocation was created (for expiration)
    pub created_at: Instant,
}

/// Default TTL for pending allocations (30 seconds)
/// If a proposal hasn't been committed within this time, the allocation expires.
const PENDING_ALLOCATION_TTL: Duration = Duration::from_secs(30);

/// Tracks all pending allocations on the leader.
///
/// This is a leader-only structure that prevents the double-spend race condition
/// in the "Smart Leader" pattern. When the leader proposes a volume creation,
/// it registers a pending allocation. The scheduler then accounts for these
/// pending allocations when selecting nodes for subsequent proposals.
///
/// ## Thread Safety
/// Uses a Mutex because allocations are typically short-lived and concurrent
/// access should be infrequent (only during propose operations on the leader).
///
/// ## Lifecycle
/// 1. `register()` - Called before proposing CreateVolume
/// 2. `release()` - Called when command is committed (in `apply()`)
/// 3. `cleanup_expired()` - Called periodically to remove stale allocations
#[derive(Debug, Default)]
pub struct PendingAllocations {
    /// Map from volume_id to pending allocation
    allocations: Mutex<HashMap<String, PendingAllocation>>,
}

impl PendingAllocations {
    pub fn new() -> Self {
        Self {
            allocations: Mutex::new(HashMap::new()),
        }
    }

    /// Register a pending allocation for a volume being proposed.
    ///
    /// # Arguments
    /// - `volume_id`: The volume being created
    /// - `size_bytes`: Size of the volume
    /// - `node_ids`: Nodes selected for this volume
    pub fn register(&self, volume_id: String, size_bytes: u64, node_ids: Vec<u64>) {
        let mut allocs = self.allocations.lock().unwrap();

        // Clean up expired allocations while we have the lock
        let now = Instant::now();
        allocs.retain(|_, alloc| now.duration_since(alloc.created_at) < PENDING_ALLOCATION_TTL);

        debug!(
            volume_id = %volume_id,
            size_bytes = size_bytes,
            node_ids = ?node_ids,
            "registering pending allocation"
        );

        allocs.insert(
            volume_id.clone(),
            PendingAllocation {
                volume_id,
                size_bytes,
                node_ids,
                created_at: now,
            },
        );
    }

    /// Release a pending allocation when a command is committed.
    ///
    /// # Arguments
    /// - `volume_id`: The volume that was committed
    pub fn release(&self, volume_id: &str) {
        let mut allocs = self.allocations.lock().unwrap();
        if allocs.remove(volume_id).is_some() {
            debug!(volume_id = %volume_id, "released pending allocation");
        }
    }

    /// Get the total pending allocation size for a specific node.
    ///
    /// This is used by the scheduler to account for in-flight proposals
    /// when calculating available capacity.
    pub fn pending_size_for_node(&self, node_id: u64) -> u64 {
        let allocs = self.allocations.lock().unwrap();
        let now = Instant::now();

        allocs
            .values()
            .filter(|alloc| {
                // Only count non-expired allocations
                now.duration_since(alloc.created_at) < PENDING_ALLOCATION_TTL
                    && alloc.node_ids.contains(&node_id)
            })
            .map(|alloc| alloc.size_bytes)
            .sum()
    }

    /// Check if a volume has a pending allocation.
    pub fn has_pending(&self, volume_id: &str) -> bool {
        let allocs = self.allocations.lock().unwrap();
        allocs.contains_key(volume_id)
    }

    /// Get all pending allocations (for debugging/monitoring).
    pub fn get_all(&self) -> Vec<PendingAllocation> {
        let allocs = self.allocations.lock().unwrap();
        allocs.values().cloned().collect()
    }

    /// Clean up expired allocations.
    pub fn cleanup_expired(&self) {
        let mut allocs = self.allocations.lock().unwrap();
        let now = Instant::now();
        let before = allocs.len();
        allocs.retain(|_, alloc| now.duration_since(alloc.created_at) < PENDING_ALLOCATION_TTL);
        let removed = before - allocs.len();
        if removed > 0 {
            debug!(removed = removed, "cleaned up expired pending allocations");
        }
    }
}

/// The State Machine wrapper ensuring thread safety.
pub struct Registry {
    state: Arc<RwLock<ClusterState>>,
    /// Tracks pending allocations for in-flight proposals (leader-only).
    ///
    /// This prevents the "Smart Leader" double-spend race condition where
    /// concurrent proposals could over-provision nodes.
    pending_allocations: Arc<PendingAllocations>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ClusterState::default())),
            pending_allocations: Arc::new(PendingAllocations::new()),
        }
    }

    /// Returns a snapshot of the current state (for Readers)
    pub fn get_state(&self) -> ClusterState {
        self.state.read().unwrap().clone()
    }

    /// Returns a reference to the pending allocations tracker.
    ///
    /// Used by the scheduler to account for in-flight proposals.
    pub fn pending_allocations(&self) -> &PendingAllocations {
        &self.pending_allocations
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
                    "Apply: Create Volume {} ({} GB) with replicas {:?}",
                    req.volume_id,
                    req.size_bytes / 1024 / 1024 / 1024,
                    req.replicas
                );
                // Phase 9.5: Use the replicas baked into the command by the Leader's Scheduler
                // This ensures determinism - we no longer calculate placement here.
                // The log is the single source of truth.
                let replicas = req.replicas.clone();

                if replicas.len() < req.replication_factor as usize {
                    warn!(
                        "Replica count mismatch: got {}, expected {}",
                        replicas.len(),
                        req.replication_factor
                    );
                }

                // Release the pending allocation now that it's committed
                self.pending_allocations.release(&req.volume_id);

                state.volumes.insert(
                    req.volume_id.clone(),
                    VolumeMetadata {
                        id: req.volume_id,
                        size: req.size_bytes,
                        replicas,
                        source_capsule_id: req.source_capsule_id.clone(),
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
///
/// # Arguments
/// - `id`: Volume identifier
/// - `size`: Size in bytes
/// - `replication_factor`: Number of replicas
/// - `replicas`: Pre-selected node IDs (Phase 9.5+)
/// - `source_capsule_id`: Optional source snapshot to hydrate from (Phase 9.6+)
///
/// NOTE: For Phase 9.5+, the `replicas` should be selected by the Scheduler
/// on the Leader before proposing. This ensures deterministic replay.
pub fn build_create_volume_cmd(
    id: &str,
    size: u64,
    replication_factor: u32,
    replicas: Vec<u64>,
) -> Vec<u8> {
    build_create_volume_cmd_with_source(id, size, replication_factor, replicas, None)
}

/// Build a CreateVolume command with optional source capsule for hydration
///
/// # Arguments
/// - `id`: Volume identifier
/// - `size`: Size in bytes
/// - `replication_factor`: Number of replicas
/// - `replicas`: Pre-selected node IDs (Phase 9.5+)
/// - `source_capsule_id`: Optional source snapshot to hydrate from (Phase 9.6+)
pub fn build_create_volume_cmd_with_source(
    id: &str,
    size: u64,
    replication_factor: u32,
    replicas: Vec<u64>,
    source_capsule_id: Option<String>,
) -> Vec<u8> {
    use prost::Message;

    let cmd = rpc::Command {
        payload: Some(rpc::command::Payload::CreateVolume(rpc::CreateVolume {
            volume_id: id.to_string(),
            size_bytes: size,
            replication_factor,
            replicas,
            source_capsule_id,
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

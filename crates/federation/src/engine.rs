//! Raft consensus engine for control plane coordination.
//!
//! This module provides a minimal Raft implementation for Phase 9.1,
//! using tikv/raft-rs with in-memory storage. Future phases will add
//! persistence, snapshots, and integration with the federation bridge.

use anyhow::{anyhow, Context, Result};
use raft::prelude::*;
use raft::storage::{MemStorage, Storage};
use raft::StateRole;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

/// Trait for storage implementations that support explicit persistence.
///
/// This trait extends the read-only `Storage` trait with mutation methods
/// needed for crash-safe Raft operation. Entries and hard state MUST be
/// persisted to stable storage before calling `RawNode::advance()`.
pub trait PersistentStorage: Storage {
    /// Persist log entries to stable storage.
    fn persist_entries(&mut self, entries: &[Entry]) -> Result<()>;

    /// Persist hard state (term, vote, commit) to stable storage.
    fn persist_hardstate(&mut self, hs: &HardState) -> Result<()>;

    /// Apply a snapshot to stable storage.
    fn apply_snapshot_to_storage(&mut self, snapshot: &Snapshot) -> Result<()>;
}

impl PersistentStorage for MemStorage {
    fn persist_entries(&mut self, entries: &[Entry]) -> Result<()> {
        // MemStorage uses interior mutability via RwLock
        self.wl()
            .append(entries)
            .context("failed to append entries to MemStorage")
    }

    fn persist_hardstate(&mut self, hs: &HardState) -> Result<()> {
        self.wl().set_hardstate(hs.clone());
        Ok(())
    }

    fn apply_snapshot_to_storage(&mut self, snapshot: &Snapshot) -> Result<()> {
        self.wl()
            .apply_snapshot(snapshot.clone())
            .context("failed to apply snapshot to MemStorage")
    }
}

impl PersistentStorage for crate::storage::SledStorage {
    fn persist_entries(&mut self, entries: &[Entry]) -> Result<()> {
        self.append(entries)
    }

    fn persist_hardstate(&mut self, hs: &HardState) -> Result<()> {
        self.set_hardstate(hs.clone())
    }

    fn apply_snapshot_to_storage(&mut self, snapshot: &Snapshot) -> Result<()> {
        self.apply_snapshot(snapshot.clone())
    }
}

/// Configuration for RaftEngine.
#[derive(Debug, Clone)]
pub struct RaftEngineConfig {
    /// Unique node ID in the cluster.
    pub id: u64,
    /// List of all peer IDs in the cluster (including self).
    pub peers: Vec<u64>,
}

/// Async Raft engine for control plane consensus.
///
/// This engine wraps tikv/raft-rs's `RawNode` and provides an async-friendly
/// interface for participating in a Raft cluster. It handles:
/// - Periodic ticking for heartbeats and elections
/// - Processing incoming Raft messages
/// - Sending outgoing messages to peers
/// - Persisting log entries and hard state
/// - Applying committed entries
///
/// # Phase 9.2 Update
/// - Generic over Storage trait (supports both MemStorage and SledStorage)
/// - Can use gRPC transport for network communication
/// - Committed entries are logged but not applied to a state machine
///
/// # Crash Safety
/// The engine follows the tikv/raft-rs recommended Ready handling order:
/// 1. Persist entries to stable storage
/// 2. Persist hard state to stable storage
/// 3. Apply snapshot (if any)
/// 4. Advance Raft state (signals persistence complete)
/// 5. Send messages to peers
/// 6. Apply committed entries to state machine
///
/// This ordering ensures that if a crash occurs at any point, the node
/// can recover to a consistent state on restart.
pub struct RaftEngine<S: PersistentStorage = MemStorage> {
    /// The core Raft state machine (wrapped in Mutex because RawNode is !Send).
    raft: Arc<Mutex<RawNode<S>>>,
    /// Inbox for receiving Raft messages from other nodes.
    inbox: mpsc::Receiver<Message>,
    /// Outbox for sending Raft messages to other nodes.
    /// Format: (to_node_id, message)
    outbox: mpsc::Sender<(u64, Message)>,
    /// Shutdown signal channel.
    shutdown: mpsc::Receiver<()>,
    /// Optional registry for applying state machine commands (Phase 9.3)
    registry: Option<Arc<crate::registry::Registry>>,
}

impl<S: PersistentStorage> RaftEngine<S> {
    /// Create a new RaftEngine instance with the provided storage.
    ///
    /// # Arguments
    /// - `config`: Configuration including node ID and peer list
    /// - `storage`: The storage implementation (MemStorage or SledStorage)
    /// - `inbox`: Channel for receiving messages from other nodes
    /// - `outbox`: Channel for sending messages to other nodes (format: (to_id, msg))
    /// - `shutdown`: Channel for receiving shutdown signal
    /// - `registry`: Optional registry for applying state machine commands
    ///
    /// # Errors
    /// Returns an error if:
    /// - The Raft config is invalid
    /// - The RawNode cannot be created
    pub fn new(
        config: RaftEngineConfig,
        storage: S,
        inbox: mpsc::Receiver<Message>,
        outbox: mpsc::Sender<(u64, Message)>,
        shutdown: mpsc::Receiver<()>,
        registry: Option<Arc<crate::registry::Registry>>,
    ) -> Result<Self> {
        // Create Raft config with conservative timings
        let cfg = Config {
            id: config.id,
            election_tick: 10, // 10 * 100ms = 1 second election timeout
            heartbeat_tick: 3, // 3 * 100ms = 300ms heartbeat interval
            ..Default::default()
        };
        cfg.validate().context("invalid raft config")?;

        // Create a no-op slog logger (raft crate requires slog)
        let logger = slog::Logger::root(slog::Discard, slog::o!());

        // Create RawNode with provided storage
        let raft = RawNode::new(&cfg, storage, &logger).context("failed to create raft node")?;

        info!(
            id = config.id,
            peers = ?config.peers,
            "created raft engine"
        );

        Ok(Self {
            raft: Arc::new(Mutex::new(raft)),
            inbox,
            outbox,
            shutdown,
            registry,
        })
    }

    /// Main async event loop. Runs until shutdown signal is received.
    ///
    /// The loop handles three event types:
    /// 1. **Tick events** (every 100ms): Drive Raft's internal timers
    /// 2. **Incoming messages**: Process messages from other nodes
    /// 3. **Shutdown signal**: Gracefully exit
    ///
    /// After each event, the ready state is processed to handle:
    /// - Sending messages to peers
    /// - Persisting log entries
    /// - Applying committed entries
    ///
    /// # Errors
    /// Returns an error if critical Raft operations fail (e.g., stepping, persisting).
    pub async fn run(mut self) -> Result<()> {
        let id = self.id();
        info!(id, "starting raft engine event loop");

        let mut tick_interval = interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    self.handle_tick()?;
                }

                Some(msg) = self.inbox.recv() => {
                    self.handle_message(msg)?;
                }

                _ = self.shutdown.recv() => {
                    info!(id, "raft engine shutting down");
                    break;
                }
            }

            // Process ready state after each event
            self.handle_ready().await?;
        }

        Ok(())
    }

    /// Propose a command to the Raft log.
    ///
    /// This can only succeed if the current node is the leader.
    /// The proposal will be replicated to followers and eventually
    /// committed if a quorum acknowledges it.
    ///
    /// # Arguments
    /// - `data`: The command payload to propose
    ///
    /// # Errors
    /// Returns an error if:
    /// - This node is not the leader
    /// - The proposal fails to be queued
    pub async fn propose(&self, data: Vec<u8>) -> Result<()> {
        let mut raft = self.raft.lock().expect("raft mutex poisoned");
        let data_len = data.len();

        raft.propose(vec![], data)
            .context("failed to propose to raft")?;

        debug!(id = raft.raft.id, bytes = data_len, "proposed entry");

        Ok(())
    }

    /// Propose a CreateVolume command with intelligent scheduling (Phase 9.5).
    ///
    /// This method implements the "Smart Leader / Deterministic Follower" pattern:
    /// 1. Leader runs the Scheduler to select optimal nodes (accounting for pending allocations)
    /// 2. Register pending allocation to prevent double-spend race condition
    /// 3. Selected nodes are baked into the CreateVolume command
    /// 4. Command is proposed to Raft log
    /// 5. All followers replay deterministically using the pre-selected nodes
    /// 6. Pending allocation is released when command is committed
    ///
    /// This keeps the state machine simple and ensures all nodes converge
    /// to the same state while preventing over-provisioning from concurrent requests.
    ///
    /// # Arguments
    /// - `vol_id`: Volume identifier
    /// - `size`: Volume size in bytes
    /// - `replicas`: Number of replicas
    ///
    /// # Errors
    /// Returns an error if:
    /// - No registry is attached to this engine
    /// - Scheduler cannot find sufficient nodes (including pending allocations)
    /// - Proposal fails
    ///
    /// # Concurrency Safety
    /// This method uses pending allocations tracking to prevent the "Smart Leader"
    /// double-spend race condition. When two concurrent requests arrive:
    /// - Request A registers pending allocation, selects nodes, proposes
    /// - Request B sees Request A's pending allocation, selects different nodes or fails
    /// - Both requests are processed correctly without over-provisioning
    ///
    /// # Example
    /// ```no_run
    /// # use federation::{RaftEngine, RaftEngineConfig};
    /// # use federation::Registry;
    /// # use std::sync::Arc;
    /// # async fn example(engine: RaftEngine) -> anyhow::Result<()> {
    /// // Propose a 10 GB volume with 3 replicas
    /// engine.propose_create_volume(
    ///     "vol-1".to_string(),
    ///     10 * 1024 * 1024 * 1024,
    ///     3
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn propose_create_volume(
        &self,
        vol_id: String,
        size: u64,
        replicas: u32,
    ) -> Result<()> {
        use prost::Message;

        // 1. Get cluster state snapshot and pending allocations from the registry
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| anyhow!("No registry attached to RaftEngine"))?;

        let state = registry.get_state();
        let pending = registry.pending_allocations();

        // 2. Run the Scheduler to select optimal nodes (accounting for pending allocations)
        // This prevents the "Smart Leader" double-spend race condition
        let requirements = crate::scheduler::PlacementRequirements {
            size_bytes: size,
            replication_factor: replicas,
            required_tags: std::collections::HashMap::new(),
        };

        let selected_nodes =
            crate::scheduler::Scheduler::select_nodes_with_pending(&state, &requirements, pending)?;

        // 3. Register pending allocation BEFORE proposing
        // This ensures subsequent concurrent requests see this allocation
        pending.register(vol_id.clone(), size, selected_nodes.clone());

        info!(
            volume_id = %vol_id,
            size_gb = size / (1024 * 1024 * 1024),
            replication_factor = replicas,
            selected_nodes = ?selected_nodes,
            "proposing create volume with scheduled placement (pending-aware)"
        );

        // 4. Build the command with pre-selected nodes
        let cmd = crate::rpc::Command {
            payload: Some(crate::rpc::command::Payload::CreateVolume(
                crate::rpc::CreateVolume {
                    volume_id: vol_id.clone(),
                    size_bytes: size,
                    replication_factor: replicas,
                    replicas: selected_nodes,
                    source_capsule_id: None,
                },
            )),
        };

        // 5. Propose to Raft
        // Note: Pending allocation will be released in Registry::apply() when committed
        // If proposal fails, the pending allocation will eventually expire (TTL: 30s)
        let result = self.propose(cmd.encode_to_vec()).await;

        // If proposal fails immediately, release the pending allocation
        if result.is_err() {
            pending.release(&vol_id);
        }

        result
    }

    /// Check if this node is currently the leader.
    pub fn is_leader(&self) -> bool {
        let raft = self.raft.lock().expect("raft mutex poisoned");
        raft.raft.state == StateRole::Leader
    }

    /// Get the current Raft term.
    pub fn current_term(&self) -> u64 {
        let raft = self.raft.lock().expect("raft mutex poisoned");
        raft.raft.term
    }

    /// Get the current leader ID (if known).
    ///
    /// Returns `None` if no leader has been elected yet.
    pub fn leader_id(&self) -> Option<u64> {
        let raft = self.raft.lock().expect("raft mutex poisoned");
        if raft.raft.leader_id == 0 {
            None
        } else {
            Some(raft.raft.leader_id)
        }
    }

    /// Get this node's ID.
    fn id(&self) -> u64 {
        let raft = self.raft.lock().expect("raft mutex poisoned");
        raft.raft.id
    }

    /// Handle a tick event (called every 100ms).
    fn handle_tick(&mut self) -> Result<()> {
        let mut raft = self.raft.lock().expect("raft mutex poisoned");
        raft.tick();
        Ok(())
    }

    /// Handle an incoming Raft message from another node.
    fn handle_message(&mut self, msg: Message) -> Result<()> {
        let mut raft = self.raft.lock().expect("raft mutex poisoned");
        raft.step(msg).context("failed to step raft with message")?;
        Ok(())
    }

    /// Process the ready state from Raft.
    ///
    /// This is the core of the Raft engine. It handles Ready state in the
    /// crash-safe order required by tikv/raft-rs:
    ///
    /// 1. Persist entries to stable storage (BEFORE advance)
    /// 2. Persist hard state to stable storage (BEFORE advance)
    /// 3. Apply snapshot if present (BEFORE advance)
    /// 4. Advance Raft state (signals persistence complete)
    /// 5. Send messages to peers (can fail/retry, Raft handles duplicates)
    /// 6. Apply committed entries to state machine
    ///
    /// # Critical: Ordering Guarantees
    /// - Persistence MUST complete before `advance()` is called
    /// - If we crash after `advance()` but before sending messages, Raft
    ///   will resend on recovery (the protocol handles duplicates)
    /// - Committed entries are applied after advance to ensure they're durable
    ///
    /// # Critical: Mutex Management
    /// This function carefully manages the mutex to avoid holding it
    /// across await points, which would cause runtime panics.
    async fn handle_ready(&mut self) -> Result<()> {
        // Phase 1: Extract ready state and persist (synchronous, holding lock)
        let (messages, light_messages, committed_info) = {
            let mut raft = self.raft.lock().expect("raft mutex poisoned");

            if !raft.has_ready() {
                return Ok(());
            }

            let mut ready = raft.ready();
            let node_id = raft.raft.id;

            // 1. Persist entries to stable storage FIRST (crash safety)
            if !ready.entries().is_empty() {
                let entries = ready.entries();
                debug!(
                    id = node_id,
                    count = entries.len(),
                    first_index = entries.first().map(|e| e.index),
                    last_index = entries.last().map(|e| e.index),
                    "persisting entries to stable storage"
                );

                // CRITICAL: Actually persist entries before advance!
                if let Err(e) = raft.mut_store().persist_entries(entries) {
                    error!(id = node_id, error = %e, "failed to persist entries");
                    return Err(e);
                }
            }

            // 2. Persist hard state BEFORE advance (crash safety)
            if let Some(hs) = ready.hs() {
                debug!(
                    id = node_id,
                    term = hs.term,
                    vote = hs.vote,
                    commit = hs.commit,
                    "persisting hard state to stable storage"
                );

                // CRITICAL: Actually persist hard state before advance!
                if let Err(e) = raft.mut_store().persist_hardstate(hs) {
                    error!(id = node_id, error = %e, "failed to persist hard state");
                    return Err(e);
                }
            }

            // 3. Apply snapshot BEFORE advance (if present)
            if !ready.snapshot().is_empty() {
                let snapshot = ready.snapshot();
                debug!(
                    id = node_id,
                    snapshot_index = snapshot.get_metadata().index,
                    snapshot_term = snapshot.get_metadata().term,
                    "applying snapshot to stable storage"
                );

                if let Err(e) = raft.mut_store().apply_snapshot_to_storage(snapshot) {
                    error!(id = node_id, error = %e, "failed to apply snapshot");
                    return Err(e);
                }
            }

            // 4. Extract messages to send (before advance consumes ready)
            let messages = ready.take_messages();

            // 5. Extract committed entries info for later application
            let committed_info: Vec<_> = ready
                .committed_entries()
                .iter()
                .filter_map(|entry| {
                    if entry.data.is_empty() {
                        None
                    } else {
                        Some((
                            entry.index,
                            entry.term,
                            entry.data.clone(),
                            entry.data.len(),
                            String::from_utf8_lossy(&entry.data).into_owned(),
                        ))
                    }
                })
                .collect();

            // 6. ADVANCE: Signal to Raft that persistence is complete
            // This MUST happen AFTER entries/hard_state are persisted!
            let mut light_rd = raft.advance(ready);

            // 7. Extract light ready messages
            let light_messages = light_rd.take_messages();

            // 8. Advance apply index
            raft.advance_apply();

            (messages, light_messages, committed_info)
        }; // Lock is dropped here - now safe to do async work

        // Phase 2: Send messages (async, without lock)
        // If we crash here, Raft will resend these messages on recovery.
        // The protocol is designed to handle duplicate messages safely.
        for msg in messages {
            let to = msg.to;
            if let Err(e) = self.outbox.send((to, msg)).await {
                // Log but don't fail - Raft will retry on next tick
                warn!(to = to, error = %e, "failed to send raft message, will retry");
            }
        }

        // Phase 3: Apply committed entries to state machine
        let node_id = self.id();
        for (index, term, data, bytes, payload) in committed_info {
            info!(
                id = node_id,
                index = index,
                term = term,
                bytes = bytes,
                payload = %payload,
                "applying committed entry"
            );

            // Apply to state machine (Phase 9.3)
            if let Some(ref registry) = self.registry {
                if let Err(e) = registry.apply(index, &data) {
                    error!(
                        id = node_id,
                        index = index,
                        error = %e,
                        "failed to apply entry to registry"
                    );
                    // Note: We don't return error here because the entry IS committed.
                    // The state machine application failure is a separate concern.
                }
            }
        }

        // Phase 4: Send light ready messages
        for msg in light_messages {
            let to = msg.to;
            if let Err(e) = self.outbox.send((to, msg)).await {
                warn!(to = to, error = %e, "failed to send light raft message, will retry");
            }
        }

        Ok(())
    }
}

// Convenience constructors for specific storage types

impl RaftEngine<MemStorage> {
    /// Create a new RaftEngine with in-memory storage (for testing).
    ///
    /// This is a convenience method that matches the Phase 9.1 API.
    ///
    /// # Arguments
    /// - `config`: Configuration including node ID and peer list
    /// - `inbox`: Channel for receiving messages from other nodes
    /// - `outbox`: Channel for sending messages to other nodes
    /// - `shutdown`: Channel for receiving shutdown signal
    /// - `registry`: Optional registry for applying state machine commands
    pub fn new_memory(
        config: RaftEngineConfig,
        inbox: mpsc::Receiver<Message>,
        outbox: mpsc::Sender<(u64, Message)>,
        shutdown: mpsc::Receiver<()>,
        registry: Option<Arc<crate::registry::Registry>>,
    ) -> Result<Self> {
        // Create MemStorage with initial peer set
        let storage = MemStorage::new_with_conf_state(ConfState::from((
            config.peers.clone(),
            vec![], // No learners
        )));

        Self::new(config, storage, inbox, outbox, shutdown, registry)
    }
}

impl RaftEngine<crate::storage::SledStorage> {
    /// Create a new RaftEngine with persistent disk storage.
    ///
    /// This uses SledStorage backed by the sled embedded database.
    ///
    /// # Arguments
    /// - `config`: Configuration including node ID and peer list
    /// - `storage_path`: Path to the sled database directory
    /// - `inbox`: Channel for receiving messages from other nodes
    /// - `outbox`: Channel for sending messages to other nodes
    /// - `shutdown`: Channel for receiving shutdown signal
    /// - `registry`: Optional registry for applying state machine commands
    ///
    /// # Errors
    /// Returns an error if the storage cannot be created or opened.
    pub fn new_persistent(
        config: RaftEngineConfig,
        storage_path: impl AsRef<Path>,
        inbox: mpsc::Receiver<Message>,
        outbox: mpsc::Sender<(u64, Message)>,
        shutdown: mpsc::Receiver<()>,
        registry: Option<Arc<crate::registry::Registry>>,
    ) -> Result<Self> {
        // Create or open SledStorage with initial peer set
        let storage = crate::storage::SledStorage::new_with_conf_state(
            storage_path,
            ConfState::from((config.peers.clone(), vec![])),
        )?;

        Self::new(config, storage, inbox, outbox, shutdown, registry)
    }
}

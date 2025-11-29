//! PODMS Scaling Agent - Autonomous Telemetry-Driven Operations
//!
//! The scaling agent subscribes to telemetry events and triggers autonomous actions:
//! - NewCapsule → Check policy.rpo and trigger replication
//! - HeatSpike → Migrate capsule to cooler nodes
//! - CapacityThreshold → Rebalance across nodes
//! - NodeDegraded → Evacuate capsules from failing node
//!
//! Step 3 Integration: The agent now uses the PolicyCompiler to translate
//! telemetry events into concrete ScalingActions based on declarative policies.

use anyhow::{anyhow, Context, Result};
use common::podms::{NodeId, SwarmBehavior, Telemetry, TransformOps};
use common::traits::CapsuleCatalog;
use common::{CapsuleId, CompressionPolicy, EncryptionPolicy, Policy, Segment};
use encryption::keymanager::KeyManager;
use encryption::mac::{compute_mac, verify_mac};
use encryption::policy::EncryptionMetadata;
use encryption::xts::{decrypt_segment, derive_tweak_from_hash, encrypt_segment};
use nvram_sim::NvramLog;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::compiler::{MeshState, NodeInfo, PolicyCompiler, ScalingAction};
use crate::{ContentStore, MeshNode, SwarmOps};

type AgentRuntimeParts = (
    Arc<dyn CapsuleCatalog + Send + Sync>,
    Arc<RwLock<NvramLog>>,
    Option<Arc<RwLock<KeyManager>>>,
);

struct MigrationTaskCtx<C: ContentStore> {
    mesh_node: Arc<MeshNode<C>>,
    catalog: Arc<dyn CapsuleCatalog + Send + Sync>,
    nvram_log: Arc<RwLock<NvramLog>>,
    key_manager: Option<Arc<RwLock<KeyManager>>>,
    destination: NodeId,
}

/// Scaling agent that consumes telemetry and performs autonomous actions.
///
/// Step 3: Now integrates PolicyCompiler for swarm intelligence - translating
/// declarative policies into autonomous scaling behaviors.
pub struct ScalingAgent<C: ContentStore> {
    mesh_node: Arc<MeshNode<C>>,
    compiler: PolicyCompiler,
    catalog: Option<Arc<dyn CapsuleCatalog + Send + Sync>>,
    nvram_log: Option<Arc<RwLock<NvramLog>>>,
    key_manager: Option<Arc<RwLock<KeyManager>>>,
}

impl<C: ContentStore + 'static> ScalingAgent<C> {
    /// Create a new scaling agent with default policy.
    pub fn new(mesh_node: Arc<MeshNode<C>>) -> Self {
        Self {
            mesh_node,
            compiler: PolicyCompiler::with_defaults(),
            catalog: None,
            nvram_log: None,
            key_manager: None,
        }
    }

    /// Create a new scaling agent with a custom default policy.
    pub fn with_policy(mesh_node: Arc<MeshNode<C>>, default_policy: Policy) -> Self {
        Self {
            mesh_node,
            compiler: PolicyCompiler::new(default_policy),
            catalog: None,
            nvram_log: None,
            key_manager: None,
        }
    }

    /// Create a scaling agent with full runtime dependencies for data movement.
    ///
    /// This constructor wires in the capsule catalog, NvramLog, and KeyManager
    /// so migration/evacuation/rebalancing handlers can stream real data.
    pub fn with_runtime(
        mesh_node: Arc<MeshNode<C>>,
        default_policy: Policy,
        catalog: Arc<dyn CapsuleCatalog + Send + Sync>,
        nvram_log: Arc<RwLock<NvramLog>>,
        key_manager: Arc<RwLock<KeyManager>>,
    ) -> Self {
        Self {
            mesh_node,
            compiler: PolicyCompiler::new(default_policy),
            catalog: Some(catalog),
            nvram_log: Some(nvram_log),
            key_manager: Some(key_manager),
        }
    }

    /// Run the agent loop, consuming telemetry events and triggering actions.
    /// This is the main entry point for the autonomous scaling system.
    pub async fn run(&self, mut telemetry_rx: UnboundedReceiver<Telemetry>) -> Result<()> {
        info!(node_id = %self.mesh_node.id(), "scaling agent started");

        loop {
            match telemetry_rx.recv().await {
                Some(event) => {
                    if let Err(e) = self.handle_telemetry_event(event).await {
                        warn!(error = %e, "failed to handle telemetry event");
                    }
                }
                None => {
                    info!("telemetry channel closed, shutting down agent");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single telemetry event using the policy compiler.
    ///
    /// Step 3: This method now uses the PolicyCompiler to translate events
    /// into ScalingActions, then executes each action autonomously.
    async fn handle_telemetry_event(&self, event: Telemetry) -> Result<()> {
        // Extract policy from event (use default if not specified)
        let policy = match &event {
            Telemetry::NewCapsule { policy, .. } => policy.clone(),
            _ => Policy::metro_sync(), // Default for non-capsule events
        };

        // Build current mesh state snapshot for compiler
        let mesh_state = self.build_mesh_state().await?;

        // Compile telemetry event into scaling actions
        let actions = self
            .compiler
            .compile_scaling_actions(&event, &policy, &mesh_state);

        debug!(
            event_type = std::any::type_name_of_val(&event),
            action_count = actions.len(),
            "compiled scaling actions from telemetry"
        );

        // Execute each action
        for action in actions {
            if let Err(e) = self.execute_action(action).await {
                warn!(error = %e, "failed to execute scaling action");
            }
        }

        Ok(())
    }

    /// Build a snapshot of current mesh state for the compiler.
    ///
    /// This provides the compiler with topology and capacity information
    /// needed for target selection decisions.
    async fn build_mesh_state(&self) -> Result<MeshState> {
        // For Step 3, create a basic mesh state
        // In production, this would query actual node states from the mesh
        let peer_ids = self.mesh_node.discover_peers().await?;

        let mut nodes = Vec::new();
        for peer_id in peer_ids {
            // For now, create placeholder node info
            // In production: Query actual capabilities and utilization
            nodes.push((
                peer_id,
                NodeInfo {
                    zone: self.mesh_node.zone().clone(),
                    available_bytes: 1_000_000_000, // 1GB placeholder
                    used_bytes: 100_000_000,        // 10% utilization
                    network_tier: crate::NetworkTier::Premium,
                },
            ));
        }

        Ok(MeshState::new(nodes, self.mesh_node.zone().clone()))
    }

    fn runtime_handles(&self) -> Result<AgentRuntimeParts> {
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| anyhow!("capsule catalog not configured for scaling agent"))?;
        let nvram = self
            .nvram_log
            .as_ref()
            .ok_or_else(|| anyhow!("NVRAM log not configured for scaling agent"))?;

        Ok((catalog.clone(), nvram.clone(), self.key_manager.clone()))
    }

    /// Execute a compiled scaling action.
    ///
    /// This is the execution layer - each action type has its own handler
    /// that performs the actual mesh operations (replication, migration, etc).
    async fn execute_action(&self, action: ScalingAction) -> Result<()> {
        match action {
            ScalingAction::Replicate {
                capsule_id,
                strategy,
                targets,
            } => {
                self.execute_replication(capsule_id, strategy, targets)
                    .await?;
            }
            ScalingAction::Migrate {
                capsule_id,
                reason,
                destination,
                transform,
            } => {
                self.execute_migration(capsule_id, reason, destination, transform)
                    .await?;
            }
            ScalingAction::Federate { capsule_id, zone } => {
                info!(
                    capsule = %capsule_id.as_uuid(),
                    zone = %zone,
                    "phase4 federate action (agent noop)"
                );
            }
            ScalingAction::ShardEC {
                capsule_id, zones, ..
            } => {
                info!(
                    capsule = %capsule_id.as_uuid(),
                    shard_targets = zones.len(),
                    "phase4 shard action (agent noop)"
                );
            }
            ScalingAction::Evacuate {
                source_node,
                reason,
                urgency,
            } => {
                self.execute_evacuation(source_node, reason, urgency)
                    .await?;
            }
            ScalingAction::Rebalance {
                overloaded_nodes,
                underutilized_nodes,
            } => {
                self.execute_rebalancing(overloaded_nodes, underutilized_nodes)
                    .await?;
            }
        }

        Ok(())
    }

    // ========================================================================
    // Action Executors - Step 3 Implementation
    // ========================================================================
    // These methods execute compiled ScalingActions using mesh operations.

    /// Execute replication action based on compiled strategy.
    async fn execute_replication(
        &self,
        capsule_id: CapsuleId,
        strategy: crate::compiler::ReplicationStrategy,
        targets: Vec<NodeId>,
    ) -> Result<()> {
        info!(
            capsule_id = %capsule_id.as_uuid(),
            strategy = ?strategy,
            target_count = targets.len(),
            "executing replication"
        );

        use crate::compiler::ReplicationStrategy;
        match strategy {
            ReplicationStrategy::MetroSync { replica_count } => {
                // Synchronous replication for zero-RPO
                self.execute_metro_sync_replication(capsule_id, replica_count, &targets)
                    .await?;
            }
            ReplicationStrategy::AsyncWithBatching { rpo } => {
                // Async replication with batching
                debug!(rpo_secs = rpo.as_secs(), "queuing async replication");
                // TODO: Add to replication queue with RPO-based batching
                // For now, we just log that async replication would be queued
                info!(
                    capsule_id = %capsule_id.as_uuid(),
                    rpo_secs = rpo.as_secs(),
                    "async replication would be queued (not yet implemented)"
                );
            }
            ReplicationStrategy::None => {
                // No replication needed
                debug!("no replication required");
            }
        }

        Ok(())
    }

    /// Execute metro-sync replication: load segments and mirror to targets.
    async fn execute_metro_sync_replication(
        &self,
        capsule_id: CapsuleId,
        replica_count: usize,
        targets: &[NodeId],
    ) -> Result<()> {
        debug!(
            capsule_id = %capsule_id.as_uuid(),
            replica_count = replica_count,
            "performing metro-sync replication"
        );

        // Note: In a real implementation, we would need access to:
        // 1. CapsuleCatalog to lookup capsule and get segment IDs
        // 2. NvramLog to read segment data
        //
        // For now, this is a placeholder that demonstrates the flow.
        // The actual implementation would require the agent to have
        // these dependencies injected.

        info!(
            capsule_id = %capsule_id.as_uuid(),
            target_count = targets.len().min(replica_count),
            "metro-sync replication: would load and mirror segments to targets"
        );

        // Placeholder for actual implementation:
        // let capsule = self.catalog.lookup_capsule(capsule_id)?;
        // for segment_id in capsule.segments {
        //     let segment_data = self.nvram_log.read(segment_id).await?;
        //     for target in targets.iter().take(replica_count) {
        //         self.mesh_node
        //             .mirror_segment(segment_id, &segment_data, *target)
        //             .await?;
        //     }
        // }

        Ok(())
    }

    /// Execute migration action (with optional transformation).
    async fn execute_migration(
        &self,
        capsule_id: CapsuleId,
        reason: String,
        destination: NodeId,
        transform: bool,
    ) -> Result<()> {
        let (catalog, nvram_log, key_manager) = self.runtime_handles()?;
        info!(
            capsule_id = %capsule_id.as_uuid(),
            destination = %destination,
            reason = %reason,
            transform = transform,
            "starting migration"
        );

        let ctx = MigrationTaskCtx {
            mesh_node: Arc::clone(&self.mesh_node),
            catalog,
            nvram_log,
            key_manager,
            destination,
        };

        let migrated = Self::migrate_capsule_task(ctx, capsule_id, transform, &reason).await?;

        info!(
            capsule_id = %capsule_id.as_uuid(),
            destination = %destination,
            segments = migrated,
            "migration complete"
        );
        Ok(())
    }

    /// Execute evacuation action based on urgency level.
    async fn execute_evacuation(
        &self,
        source_node: NodeId,
        reason: String,
        urgency: crate::compiler::EvacuationUrgency,
    ) -> Result<()> {
        if source_node != self.mesh_node.id() {
            debug!(
                source_node = %source_node,
                local = %self.mesh_node.id(),
                "evacuation request not for this node, ignoring"
            );
            return Ok(());
        }

        let (catalog, nvram_log, key_manager) = self.runtime_handles()?;
        let targets: Vec<NodeId> = self
            .mesh_node
            .discover_peers()
            .await?
            .into_iter()
            .filter(|peer| peer != &source_node)
            .collect();

        if targets.is_empty() {
            warn!(
                source_node = %source_node,
                "no healthy peers available for evacuation"
            );
            return Ok(());
        }

        let capsules = catalog.capsules();
        if capsules.is_empty() {
            debug!("no capsules to evacuate from node");
            return Ok(());
        }

        warn!(
            source_node = %source_node,
            capsule_count = capsules.len(),
            urgency = ?urgency,
            "evacuation starting"
        );

        use crate::compiler::EvacuationUrgency;
        match urgency {
            EvacuationUrgency::Immediate => {
                let mut set: JoinSet<Result<usize>> = JoinSet::new();
                for (idx, capsule) in capsules.into_iter().enumerate() {
                    let target = targets[idx % targets.len()];
                    let ctx = MigrationTaskCtx {
                        mesh_node: Arc::clone(&self.mesh_node),
                        catalog: Arc::clone(&catalog),
                        nvram_log: Arc::clone(&nvram_log),
                        key_manager: key_manager.clone(),
                        destination: target,
                    };
                    let reason_clone = format!("{reason} (evacuation)");
                    set.spawn(async move {
                        Self::migrate_capsule_task(ctx, capsule.id, true, &reason_clone).await
                    });
                }

                while let Some(result) = set.join_next().await {
                    result??;
                }
            }
            EvacuationUrgency::Gradual => {
                for (idx, capsule) in capsules.into_iter().enumerate() {
                    let target = targets[idx % targets.len()];
                    let ctx = MigrationTaskCtx {
                        mesh_node: Arc::clone(&self.mesh_node),
                        catalog: Arc::clone(&catalog),
                        nvram_log: Arc::clone(&nvram_log),
                        key_manager: key_manager.clone(),
                        destination: target,
                    };
                    Self::migrate_capsule_task(
                        ctx,
                        capsule.id,
                        false,
                        &format!("{reason} (gradual)"),
                    )
                    .await?;
                }
            }
        }

        info!(
            source_node = %source_node,
            urgency = ?urgency,
            "evacuation complete"
        );
        Ok(())
    }

    /// Execute rebalancing action across nodes.
    async fn execute_rebalancing(
        &self,
        overloaded_nodes: Vec<NodeId>,
        underutilized_nodes: Vec<NodeId>,
    ) -> Result<()> {
        info!(
            overloaded_count = overloaded_nodes.len(),
            underutilized_count = underutilized_nodes.len(),
            "executing rebalancing"
        );

        if !overloaded_nodes.contains(&self.mesh_node.id()) {
            debug!("local node not overloaded, skipping rebalancing work");
            return Ok(());
        }

        if underutilized_nodes.is_empty() {
            warn!("no underutilized targets available for rebalancing");
            return Ok(());
        }

        let (catalog, nvram_log, key_manager) = self.runtime_handles()?;
        let capsules = catalog.capsules();
        if capsules.is_empty() {
            debug!("no capsules to rebalance");
            return Ok(());
        }

        let total_capsules = capsules.len();

        for (idx, capsule) in capsules.into_iter().enumerate() {
            let target = underutilized_nodes[idx % underutilized_nodes.len()];
            let ctx = MigrationTaskCtx {
                mesh_node: Arc::clone(&self.mesh_node),
                catalog: Arc::clone(&catalog),
                nvram_log: Arc::clone(&nvram_log),
                key_manager: key_manager.clone(),
                destination: target,
            };
            Self::migrate_capsule_task(ctx, capsule.id, false, "rebalance").await?;
        }

        info!(
            migrated = total_capsules,
            targets = underutilized_nodes.len(),
            "rebalancing actions finished"
        );
        Ok(())
    }

    fn build_encryption_metadata(segment: &Segment, len: usize) -> EncryptionMetadata {
        EncryptionMetadata {
            encryption_version: segment.encryption_version,
            key_version: segment.key_version,
            wrapped_segment_key: None,
            tweak_nonce: segment.tweak_nonce,
            integrity_tag: segment.integrity_tag,
            ciphertext_len: Some(len as u32),
        }
    }

    async fn migrate_capsule_task(
        ctx: MigrationTaskCtx<C>,
        capsule_id: CapsuleId,
        transform: bool,
        reason: &str,
    ) -> Result<usize> {
        let capsule = ctx
            .catalog
            .lookup_capsule(capsule_id)
            .with_context(|| format!("lookup capsule {}", capsule_id.as_uuid()))?;

        // Validate sovereignty and placement constraints
        capsule
            .on_migrate(ctx.destination, ctx.mesh_node.zone())
            .with_context(|| format!("sovereignty validation for {}", capsule_id.as_uuid()))?;

        let mut migrated = 0usize;
        let swarm_ops = ctx.key_manager.as_ref().map(|km| SwarmOps::new(km.clone()));

        for segment_id in capsule.segments.iter().copied() {
            let (segment_meta, mut payload) = {
                let log = ctx.nvram_log.read().await;
                let metadata = log
                    .get_segment_metadata(segment_id)
                    .with_context(|| format!("segment metadata {}", segment_id.0))?;
                let data = log
                    .read(segment_id)
                    .with_context(|| format!("read segment {}", segment_id.0))?;
                (metadata, data)
            };

            let mut encryption_meta = Self::build_encryption_metadata(&segment_meta, payload.len());

            if transform {
                let (ops, km) = match (&swarm_ops, &ctx.key_manager) {
                    (Some(ops), Some(km)) => (ops, km),
                    _ => {
                        warn!(
                            segment = segment_id.0,
                            capsule = %capsule_id.as_uuid(),
                            "transform requested but key manager unavailable"
                        );
                        continue;
                    }
                };

                if encryption_meta.is_encrypted() && encryption_meta.key_version.is_none() {
                    warn!(
                        segment = segment_id.0,
                        capsule = %capsule_id.as_uuid(),
                        "transform requested but key metadata missing"
                    );
                    continue;
                }

                let plaintext = if encryption_meta.is_encrypted() {
                    let key_version = encryption_meta.key_version.unwrap();
                    let mut guard = km.write().await;
                    let key_pair = guard.get_key(key_version)?;

                    if encryption_meta.has_integrity_tag() {
                        if let Err(err) =
                            verify_mac(&payload, &encryption_meta, key_pair.key1(), key_pair.key2())
                        {
                            warn!(
                                segment = segment_id.0,
                                capsule = %capsule_id.as_uuid(),
                                error = %err,
                                "integrity validation failed, skipping segment"
                            );
                            continue;
                        }
                    } else {
                        let mac = compute_mac(
                            &payload,
                            &encryption_meta,
                            key_pair.key1(),
                            key_pair.key2(),
                        )?;
                        encryption_meta.set_integrity_tag(mac);
                    }

                    let decrypted = decrypt_segment(&payload, key_pair, &encryption_meta)
                        .with_context(|| format!("decrypt segment {}", segment_id.0))?;
                    drop(guard);
                    decrypted
                } else {
                    payload.clone()
                };

                let src_comp =
                    compression_policy_from_segment(&segment_meta).unwrap_or_else(|| {
                        warn!(
                            segment = segment_id.0,
                            capsule = %capsule_id.as_uuid(),
                            algo = %segment_meta.compression_algo,
                            "unknown compression algorithm, treating as None"
                        );
                        CompressionPolicy::None
                    });

                let target_policy = target_policy_with_rotation(&capsule.policy, km.as_ref());
                let dst_comp = target_policy.compression.clone();
                let dst_enc = target_policy.encryption.clone();

                let mut transformed = plaintext;
                if src_comp != dst_comp {
                    if !matches!(src_comp, CompressionPolicy::None) {
                        transformed = ops
                            .decompress(&transformed, &src_comp)
                            .with_context(|| format!("decompress segment {}", segment_id.0))?;
                    }
                    if !matches!(dst_comp, CompressionPolicy::None) {
                        transformed = ops
                            .compress(&transformed, &dst_comp)
                            .with_context(|| format!("compress segment {}", segment_id.0))?;
                    }
                }

                let (ciphertext, new_meta) = ops
                    .encrypt_with_metadata(capsule_id, &transformed, &dst_enc, segment_id)
                    .with_context(|| format!("encrypt segment {}", segment_id.0))?;
                payload = ciphertext;
                encryption_meta = new_meta;
            } else {
                if encryption_meta.key_version.is_none() && ctx.key_manager.is_none() {
                    warn!(
                        segment = segment_id.0,
                        capsule = %capsule_id.as_uuid(),
                        "missing key metadata and no key manager available; skipping segment"
                    );
                    continue;
                }

                if encryption_meta.key_version.is_none() {
                    if let Some(km) = ctx.key_manager.as_ref() {
                        let mut guard = km.write().await;
                        let target_version = guard.current_version();
                        let target_pair = guard.get_key(target_version)?;
                        let tweak = derive_tweak_from_hash(blake3::hash(&payload).as_bytes());
                        let (ciphertext, mut new_meta) =
                            encrypt_segment(&payload, target_pair, target_version, tweak)?;
                        let mac = compute_mac(
                            &ciphertext,
                            &new_meta,
                            target_pair.key1(),
                            target_pair.key2(),
                        )?;
                        new_meta.set_integrity_tag(mac);
                        new_meta.ciphertext_len = Some(ciphertext.len() as u32);
                        payload = ciphertext;
                        encryption_meta = new_meta;
                    }
                }

                if let Some(km) = ctx.key_manager.as_ref() {
                    let mut guard = km.write().await;
                    if let Some(key_version) = encryption_meta.key_version {
                        let key_pair = guard.get_key(key_version)?;

                        // Validate MAC if present
                        if encryption_meta.has_integrity_tag() {
                            if let Err(err) = verify_mac(
                                &payload,
                                &encryption_meta,
                                key_pair.key1(),
                                key_pair.key2(),
                            ) {
                                warn!(
                                    segment = segment_id.0,
                                    capsule = %capsule_id.as_uuid(),
                                    error = %err,
                                    "integrity validation failed, skipping segment"
                                );
                                continue;
                            }
                        } else {
                            // Populate MAC so the receiver enforces integrity
                            let mac = compute_mac(
                                &payload,
                                &encryption_meta,
                                key_pair.key1(),
                                key_pair.key2(),
                            )?;
                            encryption_meta.set_integrity_tag(mac);
                        }
                    }
                }
            }

            let mut frame =
                crate::replication::ReplicationFrame::new(segment_id, encryption_meta, payload);
            frame.capsule_id = Some(capsule_id);
            ctx.mesh_node
                .send_replication_frame(&frame, ctx.destination)
                .await
                .with_context(|| {
                    format!("stream segment {} to {}", segment_id.0, ctx.destination)
                })?;
            migrated += 1;
        }

        info!(
            capsule = %capsule_id.as_uuid(),
            destination = %ctx.destination,
            segments = migrated,
            reason = reason,
            "migration task finished"
        );

        Ok(migrated)
    }
}

fn compression_policy_from_segment(segment: &Segment) -> Option<CompressionPolicy> {
    if !segment.compressed {
        return Some(CompressionPolicy::None);
    }

    let algo = segment.compression_algo.to_lowercase();
    if let Some(level) = algo.strip_prefix("lz4:") {
        return level
            .parse::<i32>()
            .ok()
            .map(|lvl| CompressionPolicy::LZ4 { level: lvl });
    }
    if algo == "lz4" {
        return Some(CompressionPolicy::LZ4 { level: 1 });
    }
    if let Some(level) = algo.strip_prefix("zstd:") {
        return level
            .parse::<i32>()
            .ok()
            .map(|lvl| CompressionPolicy::Zstd { level: lvl });
    }
    if algo == "zstd" {
        return Some(CompressionPolicy::Zstd { level: 0 });
    }

    None
}

fn target_policy_with_rotation(
    policy: &Policy,
    key_manager: &tokio::sync::RwLock<KeyManager>,
) -> Policy {
    let mut target = policy.clone();
    if let EncryptionPolicy::XtsAes256 { key_version } = &mut target.encryption {
        if key_version.is_none() {
            let guard = key_manager.blocking_read();
            *key_version = Some(guard.current_version());
            drop(guard);
        }
    }
    target
}

// Note: Tests removed because they need concrete ContentStore implementation
// They will be added once CapsuleRegistry implements ContentStore

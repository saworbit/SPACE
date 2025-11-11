//! NVMe-oF view projection helpers for Phase 4.
//!
//! This crate implements the "one capsule, infinite views" concept by projecting
//! capsules into NVMe-oF targets while coordinating mesh federation and metadata
//! sharding via PODMS policies.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::{SwarmBehavior, Telemetry};
use common::{CapsuleId, Policy};
use scaling::compiler::{compile_scaling, MeshState, ScalingAction};
use scaling::{MeshNode, MetadataShard};
use spdk_rs::{Namespace, NvmeTargetBuilder};
use tracing::{info, info_span};

/// Handle representing an exported NVMe view.
#[derive(Debug)]
pub struct NvmeView {
    capsule_id: CapsuleId,
    target: spdk_rs::NvmeTarget,
}

impl NvmeView {
    /// Retrieve the capsule referenced by this view.
    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }

    /// Access the underlying NVMe target (namespaces).
    pub fn nvme_target(&self) -> &spdk_rs::NvmeTarget {
        &self.target
    }
}

/// Project a capsule into an NVMe-oF target with PODMS federation.
pub async fn project_nvme_view(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<NvmeView> {
    let span = info_span!("nvme_project", capsule = %id.as_uuid());
    let _enter = span.enter();

    let capsule = registry.lookup(id)?;
    let transformed = capsule.apply_transform(&[], policy)?;

    let telemetry = Telemetry::ViewProjection {
        id,
        view: "nvme".into(),
    };

    let mesh_state = MeshState::empty(mesh.zone().clone());
    let actions = compile_scaling(policy, &telemetry, &mesh_state);

    for action in actions {
        match action {
            ScalingAction::Federate { capsule_id, zone } => {
                mesh.federate_capsule(capsule_id, zone).await?;
            }
            ScalingAction::ShardEC {
                capsule_id, zones, ..
            } => {
                if zones.is_empty() {
                    continue;
                }
                let payload = registry.serialize_capsule(capsule_id)?;
                let shard_keys = capsule_id.shard_keys(zones.len());
                let shards: Vec<MetadataShard> = zones
                    .into_iter()
                    .zip(shard_keys.into_iter())
                    .map(|(zone, shard_id)| MetadataShard {
                        shard_id,
                        owner: mesh.id(),
                        zone,
                    })
                    .collect();
                mesh.shard_metadata(capsule_id, shards, &payload).await?;
            }
            _ => {}
        }
    }

    let mut builder = NvmeTargetBuilder::new();
    builder.add_namespace(Namespace::new(transformed));
    let target = builder.build();

    info!(
        namespaces = target.namespaces().len(),
        "nvme view projected"
    );

    Ok(NvmeView {
        capsule_id: id,
        target,
    })
}

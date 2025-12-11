//! NVMe-oF view projection helpers for Phase 4.
//!
//! This crate implements the "one capsule, infinite views" concept by projecting
//! capsules into NVMe-oF targets while coordinating mesh federation and metadata
//! sharding via PODMS policies.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::{CapsuleRegistry, RegistryTransformOps};
use common::podms::SwarmBehavior;
use common::{CapsuleId, EncryptionPolicy, Policy, SegmentId};
use scaling::enforce_view_policy;
use scaling::MeshNode;
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
pub async fn project_nvme_view<C: scaling::ContentStore + 'static>(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode<C>,
    registry: &CapsuleRegistry,
) -> Result<NvmeView> {
    let span = info_span!("nvme_project", capsule = %id.as_uuid());
    let _enter = span.enter();

    enforce_view_policy(mesh, id, policy, "nvme", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

    let transform_ops = RegistryTransformOps::new(registry.key_manager().clone());

    let capsule = registry.lookup(id)?;
    let mut view_policy = policy.clone();
    view_policy.encryption = EncryptionPolicy::Disabled;
    let transformed = capsule.apply_transform(SegmentId(0), &[], &view_policy, &transform_ops)?;

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

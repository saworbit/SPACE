//! NVMe-oF view projection helpers for Phase 4.
//!
//! This crate implements the "one capsule, infinite views" concept by projecting
//! capsules into NVMe-oF targets while coordinating mesh federation and metadata
//! sharding via PODMS policies.
#![cfg(feature = "phase4")]

use anyhow::{Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{Capsule, CapsuleId, Policy};
use scaling::{enforce_view_policy, MeshNode};
use serde::Serialize;
use spdk_rs::{Bdev, BlockRange, Namespace, NvmeTarget, NvmeTargetBuilder, NvmfSubsystem};
use tracing::{info, info_span};

/// Handle representing an exported NVMe view.
#[derive(Debug)]
pub struct NvmeView {
    pub subsystem_nqn: String,
    capsule_id: CapsuleId,
    bdev: Bdev,
    subsystem: NvmfSubsystem,
    target: NvmeTarget,
}

impl NvmeView {
    /// Retrieve the capsule referenced by this view.
    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }

    /// Access the underlying NVMe target (namespaces).
    pub fn nvme_target(&self) -> &NvmeTarget {
        &self.target
    }

    /// Inspect the exported NQN.
    pub fn nqn(&self) -> &str {
        &self.subsystem_nqn
    }

    /// Access the backing bdev.
    pub fn bdev(&self) -> &Bdev {
        &self.bdev
    }
}

#[derive(Debug, Serialize)]
struct NamespaceDescriptor {
    capsule_id: CapsuleId,
    size: u64,
    segments: Vec<String>,
}

fn build_namespace(capsule: &Capsule) -> Result<Namespace> {
    let descriptor = NamespaceDescriptor {
        capsule_id: capsule.id,
        size: capsule.size,
        segments: capsule
            .segments
            .iter()
            .map(|seg| seg.0.to_string())
            .collect(),
    };
    let blob = serde_json::to_vec(&descriptor)?;
    Ok(Namespace::new(blob))
}

fn register_bdev(capsule: &Capsule, bdev_name: &str) -> Result<Bdev> {
    let segments: Vec<String> = if capsule.segments.is_empty() {
        vec!["unmapped".to_string()]
    } else {
        capsule.segments.iter().map(|s| s.0.to_string()).collect()
    };

    let mapper_segments = std::sync::Arc::new(segments);
    let mapper = move |offset: u64, len: u64| -> Vec<BlockRange> {
        let count = mapper_segments.len().max(1) as u64;
        let stride = std::cmp::max(1, len / count);
        let mut ranges = Vec::new();
        for (idx, backing) in mapper_segments.iter().enumerate() {
            let start = offset.saturating_add(stride.saturating_mul(idx as u64));
            let remaining = len.saturating_sub(stride.saturating_mul(idx as u64));
            let span = if idx == mapper_segments.len() - 1 {
                remaining
            } else {
                stride
            };
            ranges.push(BlockRange {
                offset: start,
                len: span,
                backing: backing.clone(),
            });
        }
        ranges
    };

    Bdev::register(bdev_name, capsule.size, mapper).map_err(anyhow::Error::msg)
}

/// Project a capsule into an NVMe-oF target with PODMS federation.
pub async fn project_nvme_view<C: scaling::ContentStore + 'static>(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode<C>,
    registry: &CapsuleRegistry,
) -> Result<NvmeView> {
    let capsule = registry
        .lookup(id)
        .with_context(|| format!("lookup capsule {}", id.as_uuid()))?;
    NvmeView::project(&capsule, policy, mesh, registry).await
}

impl NvmeView {
    /// Create a fully-projected NVMe view for a capsule.
    pub async fn project<C: scaling::ContentStore + 'static>(
        capsule: &Capsule,
        policy: &Policy,
        mesh: &MeshNode<C>,
        registry: &CapsuleRegistry,
    ) -> Result<Self> {
        let span = info_span!("nvme_project", capsule = %capsule.id.as_uuid());
        let _enter = span.enter();

        enforce_view_policy(mesh, capsule.id, policy, "nvme", |cid| {
            registry.serialize_capsule(cid)
        })
        .await?;

        let bdev_name = format!("capsule-{}", capsule.id.as_uuid());
        let bdev = register_bdev(capsule, &bdev_name)?;

        let nqn = format!("nqn.2025-11.io.space:{}", capsule.id.as_uuid());
        let subsystem = NvmfSubsystem::create(&nqn, bdev.name()).map_err(anyhow::Error::msg)?;

        let mut builder = NvmeTargetBuilder::new();
        builder.add_namespace(build_namespace(capsule)?);
        let target = builder.build();

        info!(
            namespaces = target.namespaces().len(),
            nqn = %nqn,
            bdev = %bdev_name,
            "nvme view projected"
        );

        Ok(Self {
            capsule_id: capsule.id,
            bdev,
            subsystem,
            target,
            subsystem_nqn: nqn,
        })
    }
}

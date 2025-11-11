//! Phase 4 NVMe-oF view projection helpers.
//! The implementation here is intentionally light: it verifies capsule existence,
//! enforces sovereignty checks, and reuses the PODMS mesh APIs added for Phase 4.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::{SovereigntyLevel, ZoneId};
use common::{CapsuleId, Policy};
use scaling::{MeshNode, MetadataShard};
use tracing::{info, info_span};

/// Handle representing an NVMe target exported from SPACE.
pub struct NvmeTarget {
    capsule_id: CapsuleId,
    namespace: ZoneId,
}

impl NvmeTarget {
    /// The capsule referenced by this NVMe namespace.
    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }
}

/// Project a capsule as an NVMe-oF target.
pub async fn project_nvme_view(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<NvmeTarget> {
    let span = info_span!("nvme_project", capsule = %id.as_uuid());
    let _enter = span.enter();

    registry.lookup(id)?;

    if policy.sovereignty != SovereigntyLevel::Local {
        let remote = mesh.resolve_federated(id).await?;
        info!(
            capsule = %id.as_uuid(),
            target = %remote,
            "resolving federated NVMe view target"
        );
        mesh.federate_capsule(id, mesh.zone().clone()).await?;
    }

    mesh.shard_metadata(
        id,
        vec![MetadataShard {
            shard_id: 1,
            owner: mesh.id(),
        }],
    )
    .await?;

    Ok(NvmeTarget {
        capsule_id: id,
        namespace: mesh.zone().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_registry::CapsuleRegistry;
    use common::Policy;
    use scaling::MeshNode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn phantom_nvme_target_reports_capsule() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::default();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = MeshNode::new(
            common::podms::ZoneId::Metro {
                name: "test".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .unwrap();

        let target = project_nvme_view(capsule_id, &policy, &mesh, &registry).await;
        assert!(target.is_ok());
    }
}

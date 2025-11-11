//! Phase 4 NFS exports.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::ZoneId;
use common::{CapsuleId, Policy};
use scaling::MeshNode;
use std::time::Duration;
use tracing::{info, info_span};

/// Descriptor returned to callers after exporting a capsule over NFS.
pub struct NfsExport {
    capsule_id: CapsuleId,
    export_path: String,
}

impl NfsExport {
    /// The capsule associated with this export.
    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }
}

/// Exports a capsule via a Phase 4 NFS view.
pub async fn export_nfs_view(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<NfsExport> {
    let span = info_span!("nfs_export", capsule = %id.as_uuid());
    let _enter = span.enter();

    registry.lookup(id)?;

    if policy.latency_target < Duration::from_millis(2) {
        info!(
            capsule = %id.as_uuid(),
            target_zone = %mesh.zone(),
            "latency target triggered metro federation"
        );
        mesh.federate_capsule(id, mesh.zone().clone()).await?;
    }

    let export_path = format!("/capsules/{}", id.as_uuid());
    info!(capsule = %id.as_uuid(), export_path, "registered NFS export");

    Ok(NfsExport {
        capsule_id: id,
        export_path,
    })
}

#[cfg(all(test, feature = "phase4"))]
mod tests {
    use super::*;
    use capsule_registry::CapsuleRegistry;
    use common::Policy;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn exports_nfs_path_for_capsule() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::default();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = MeshNode::new(
            ZoneId::Metro {
                name: "phase4".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .unwrap();

        let export = export_nfs_view(capsule_id, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert_eq!(export.capsule_id(), capsule_id);
    }
}

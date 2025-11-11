//! Phase 4 CSI provisioning helpers.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::SovereigntyLevel;
use common::{CapsuleId, Policy};
use scaling::{MeshNode, MetadataShard};
use tracing::{info, info_span};

/// Simple representation of a CSI provisioned volume.
pub struct CsiVolume {
    capsule_id: CapsuleId,
    driver_path: String,
}

impl CsiVolume {
    /// Retrieve the capsule ID associated with the volume.
    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }
}

/// Provision a CSI volume backed by SPACE capsules.
pub async fn csi_provision_volume(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<CsiVolume> {
    let span = info_span!("csi_provision", capsule = %id.as_uuid());
    let _enter = span.enter();

    registry.lookup(id)?;

    if policy.sovereignty == SovereigntyLevel::Zone {
        let target = mesh.resolve_federated(id).await?;
        info!(
            capsule = %id.as_uuid(),
            target = %target,
            "federating CSI volume to zone peer"
        );
        mesh.federate_capsule(id, mesh.zone().clone()).await?;
    }

    mesh.shard_metadata(
        id,
        vec![MetadataShard {
            shard_id: 42,
            owner: mesh.id(),
        }],
    )
    .await?;

    Ok(CsiVolume {
        capsule_id: id,
        driver_path: format!("/csi/volumes/{}", id.as_uuid()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_registry::CapsuleRegistry;
    use common::Policy;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn csi_volume_roundtrip_honors_mesh() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::default();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = MeshNode::new(
            common::podms::ZoneId::Metro {
                name: "zone".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .unwrap();

        let volume = csi_provision_volume(capsule_id, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert_eq!(volume.capsule_id(), capsule_id);
    }
}

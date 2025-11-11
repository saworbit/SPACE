//! Phase 4 NFS exports.

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::Telemetry;
use common::{CapsuleId, Policy};
use nfs_rs::{ExportOptions, NfsServer};
use scaling::compiler::{compile_scaling, MeshState, ScalingAction};
use scaling::{MeshNode, MetadataShard};
use tracing::{info, info_span};

/// Export a capsule via a Phase 4 NFS view.
pub async fn export_nfs_view(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<NfsServer> {
    let span = info_span!("nfs_export", capsule = %id.as_uuid());
    let _enter = span.enter();

    registry.lookup(id)?;

    let telemetry = Telemetry::ViewProjection {
        id,
        view: "nfs".into(),
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

    let export_path = format!("/capsules/{}", id.as_uuid());
    let mut server = NfsServer::new();
    server.export(
        id.as_uuid().to_string(),
        ExportOptions::new(export_path.clone()),
    );

    info!(capsule = %id.as_uuid(), export_path, "registered NFS export");

    server.start().await
}

#[cfg(all(test, feature = "phase4"))]
mod tests {
    use super::*;
    use capsule_registry::CapsuleRegistry;
    use common::podms::ZoneId;
    use common::Policy;
    use scaling::MeshNode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn exports_nfs_target() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
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

        let server = export_nfs_view(capsule_id, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert!(server.start().await.is_ok());
    }
}

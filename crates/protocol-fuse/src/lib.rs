//! Phase 4 FUSE view projections with federated metadata.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::podms::{SwarmBehavior, Telemetry};
use common::{CapsuleId, Policy};
use fuse_rs::{FilesystemImpl, MountHandle};
use scaling::compiler::{compile_scaling, MeshState, ScalingAction};
use scaling::{MeshNode, MetadataShard};
use tracing::{info, info_span};

/// Mounts a capsule as a local FUSE view with Phase 4 federation.
pub async fn mount_fuse_view(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode,
    mountpoint: &str,
    registry: &CapsuleRegistry,
) -> Result<MountHandle> {
    let span = info_span!("fuse_mount", capsule = %id.as_uuid(), mountpoint);
    let _enter = span.enter();

    let capsule = registry.lookup(id)?;
    let transformed = capsule.apply_transform(&[], policy)?;

    let telemetry = Telemetry::ViewProjection {
        id,
        view: "fuse".into(),
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

    let fs = FilesystemImpl::new(transformed);
    let handle = fs.mount(mountpoint)?;
    info!(capsule = %id.as_uuid(), mountpoint, "mounted FUSE view");
    Ok(handle)
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
    async fn fuse_mount_returns_handle() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = MeshNode::new(
            ZoneId::Metro {
                name: "fuse-test".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .unwrap();

        let handle = mount_fuse_view(capsule_id, &policy, &mesh, "/tmp/space", &registry)
            .await
            .unwrap();
        assert!(!handle.mountpoint().is_empty());
    }
}

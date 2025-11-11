//! Phase 4 CSI provisioning with federated metadata and mesh sharding.
#![cfg(feature = "phase4")]

use anyhow::{anyhow, Result};
use capsule_registry::CapsuleRegistry;
use common::podms::Telemetry;
use common::{CapsuleId, Policy};
use csi_driver_rs::{CsiServer, ProvisionRequest};
use scaling::compiler::{compile_scaling, MeshState, ScalingAction};
use scaling::{MeshNode, MetadataShard};
use tracing::info_span;
use uuid::Uuid;

/// Provision a CSI volume backed by a SPACE capsule.
pub async fn csi_provision_capsule(
    req: ProvisionRequest,
    policy: &Policy,
    mesh: &MeshNode,
    registry: &CapsuleRegistry,
) -> Result<CsiServer> {
    let span = info_span!("csi_provision", request = ?req);
    let _enter = span.enter();

    let id = CapsuleId::from_uuid(Uuid::parse_str(&req.capsule_id).map_err(|e| anyhow!(e))?);
    registry.lookup(id)?;

    let telemetry = Telemetry::ViewProjection {
        id,
        view: "csi".into(),
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

    CsiServer::provision(&id.as_uuid().to_string())
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
    async fn provisions_csi_volume() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = MeshNode::new(
            ZoneId::Metro {
                name: "csi-test".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .unwrap();

        let req = ProvisionRequest::from_capsule(&capsule_id.as_uuid().to_string());
        let server = csi_provision_capsule(req, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert_eq!(server.capsule_id(), capsule_id.as_uuid().to_string());
    }
}

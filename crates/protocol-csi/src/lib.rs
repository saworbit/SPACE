//! Phase 4 CSI provisioning with federated metadata and mesh sharding.
#![cfg(feature = "phase4")]

use anyhow::{anyhow, Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{CapsuleId, Policy};
use csi_driver_rs::{CsiServer, ProvisionRequest};
use protocol_fuse::mount_fuse_view;
use scaling::{enforce_view_policy, MeshNode};
use tracing::info_span;
use uuid::Uuid;

/// Provision a CSI volume backed by a SPACE capsule.
pub async fn csi_provision_capsule<C: scaling::ContentStore + 'static>(
    req: ProvisionRequest,
    policy: &Policy,
    mesh: &MeshNode<C>,
    registry: &CapsuleRegistry,
) -> Result<CsiServer> {
    let span = info_span!("csi_provision", request = ?req);
    let _enter = span.enter();

    let id = CapsuleId::from_uuid(Uuid::parse_str(&req.capsule_id).map_err(|e| anyhow!(e))?);
    registry.lookup(id)?;

    enforce_view_policy(mesh, id, policy, "csi", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

    CsiServer::provision(&id.as_uuid().to_string())
}

/// CSI Node helper that mounts capsules via the FUSE protocol view.
pub async fn publish_capsule_volume<C: scaling::ContentStore + 'static>(
    volume_id: &str,
    target_path: &str,
    policy: &Policy,
    mesh: &MeshNode<C>,
    registry: &CapsuleRegistry,
) -> Result<fuse_rs::MountHandle> {
    let capsule_id = CapsuleId::from_uuid(
        Uuid::parse_str(volume_id).with_context(|| format!("invalid capsule UUID: {volume_id}"))?,
    );
    enforce_view_policy(mesh, capsule_id, policy, "csi", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

    mount_fuse_view(capsule_id, policy, mesh, target_path, registry).await
}

#[cfg(all(test, feature = "phase4"))]
mod tests {
    use super::*;
    use capsule_registry::CapsuleRegistry;
    use common::podms::ZoneId;
    use common::{CapsuleId, ContentHash, Policy, SegmentId};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    use scaling::{ContentStore, MeshNode};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    #[derive(Default)]
    struct DummyContentStore;

    impl ContentStore for DummyContentStore {
        fn lookup_content(&self, _hash: &ContentHash) -> Option<SegmentId> {
            None
        }

        fn register_content(&self, _hash: &ContentHash, _segment_id: SegmentId) {}
    }

    async fn build_mesh(zone: ZoneId) -> MeshNode<DummyContentStore> {
        let content = Arc::new(RwLock::new(DummyContentStore));
        let nvram_path =
            std::env::temp_dir().join(format!("csi-mesh-{}.log", CapsuleId::new().as_uuid()));
        let nvram = Arc::new(RwLock::new(
            NvramLog::open(nvram_path.to_string_lossy().as_ref()).expect("open nvram"),
        ));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));
        MeshNode::new(
            zone,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            content,
            nvram,
            key_manager,
        )
        .await
        .unwrap()
    }

    fn registry_with_tempdir() -> (CapsuleRegistry, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let registry = CapsuleRegistry::open(db_path.to_string_lossy().as_ref()).unwrap();
        (registry, dir)
    }

    #[tokio::test]
    async fn provisions_csi_volume() {
        let (registry, _tmp) = registry_with_tempdir();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = build_mesh(ZoneId::Metro {
            name: "csi-test".into(),
        })
        .await;

        let req = ProvisionRequest::from_capsule(&capsule_id.as_uuid().to_string());
        let server = csi_provision_capsule(req, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert_eq!(server.capsule_id(), capsule_id.as_uuid().to_string());
    }

    #[tokio::test]
    async fn publishes_volume_via_fuse_mount() {
        let (registry, _tmp) = registry_with_tempdir();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = build_mesh(ZoneId::Metro {
            name: "csi-publish".into(),
        })
        .await;

        let target_path = format!("/tmp/space-csi-{}", capsule_id.as_uuid());
        let handle = publish_capsule_volume(
            &capsule_id.as_uuid().to_string(),
            &target_path,
            &policy,
            &mesh,
            &registry,
        )
        .await
        .unwrap();

        assert_eq!(handle.mountpoint(), target_path);
    }
}

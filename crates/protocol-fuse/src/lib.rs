//! Phase 4 FUSE view projections with federated metadata.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::{CapsuleRegistry, RegistryTransformOps};
use common::podms::SwarmBehavior;
use common::{CapsuleId, EncryptionPolicy, Policy, SegmentId};
use fuse_rs::{FilesystemImpl, MountHandle};
use scaling::enforce_view_policy;
use scaling::MeshNode;
use tracing::{info, info_span};

/// Mounts a capsule as a local FUSE view with Phase 4 federation.
pub async fn mount_fuse_view<C: scaling::ContentStore + 'static>(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode<C>,
    mountpoint: &str,
    registry: &CapsuleRegistry,
) -> Result<MountHandle> {
    let span = info_span!("fuse_mount", capsule = %id.as_uuid(), mountpoint);
    let _enter = span.enter();

    enforce_view_policy(mesh, id, policy, "fuse", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

    let transform_ops = RegistryTransformOps::new(registry.key_manager().clone());

    let capsule = registry.lookup(id)?;
    let mut view_policy = policy.clone();
    view_policy.encryption = EncryptionPolicy::Disabled;
    let transformed = capsule.apply_transform(SegmentId(0), &[], &view_policy, &transform_ops)?;

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
    use common::{CapsuleId, ContentHash, Policy, SegmentId};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    use scaling::{ContentStore, MeshNode};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
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
            std::env::temp_dir().join(format!("fuse-mesh-{}.log", CapsuleId::new().as_uuid()));
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

    #[tokio::test]
    async fn fuse_mount_returns_handle() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = build_mesh(ZoneId::Metro {
            name: "fuse-test".into(),
        })
        .await;

        let handle = mount_fuse_view(capsule_id, &policy, &mesh, "/tmp/space", &registry)
            .await
            .unwrap();
        assert!(!handle.mountpoint().is_empty());
    }
}

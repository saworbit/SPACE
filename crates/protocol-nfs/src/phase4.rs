//! Phase 4 NFS exports.

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::{CapsuleId, Policy};
use nfs_rs::{ExportOptions, NfsServer};
use scaling::{enforce_view_policy, ContentStore, MeshNode};
use tracing::{info, info_span};

/// Export a capsule via a Phase 4 NFS view.
pub async fn export_nfs_view<C: ContentStore>(
    id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode<C>,
    registry: &CapsuleRegistry,
) -> Result<NfsServer> {
    let span = info_span!("nfs_export", capsule = %id.as_uuid());
    let _enter = span.enter();

    registry.lookup(id)?;

    enforce_view_policy(mesh, id, policy, "nfs", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

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
    use common::{Policy, SegmentId};
    use encryption::keymanager::KeyManager;
    use nvram_sim::NvramLog;
    use scaling::{ContentStore, MeshNode};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[derive(Clone, Default)]
    struct DummyContentStore;

    impl ContentStore for DummyContentStore {
        fn lookup_content(&self, _hash: &common::ContentHash) -> Option<SegmentId> {
            None
        }

        fn register_content(&self, _hash: &common::ContentHash, _segment_id: SegmentId) {}
    }

    #[tokio::test]
    async fn exports_nfs_target() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let content_store = Arc::new(RwLock::new(DummyContentStore::default()));
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("nfs_phase4_nvram.log");
        let nvram = Arc::new(RwLock::new(NvramLog::open(&log_path).unwrap()));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));

        let mesh = MeshNode::new(
            ZoneId::Metro {
                name: "phase4".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            content_store,
            nvram,
            key_manager,
        )
        .await
        .unwrap();

        let server = export_nfs_view(capsule_id, &policy, &mesh, &registry)
            .await
            .unwrap();
        assert!(server.start().await.is_ok());
    }
}

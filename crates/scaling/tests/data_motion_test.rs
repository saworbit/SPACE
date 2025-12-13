use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::podms::ZoneId;
use common::traits::{CapsuleCatalog, DedupStats};
use common::{CapsuleId, ContentHash, Policy, SegmentId};
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use pipeline::InMemoryCatalog;
use scaling::agent::{MotionMode, ScalingAgent};
use scaling::{ContentStore, MeshNode};
use tokio::sync::RwLock;
use tokio::time::sleep;

#[derive(Clone, Default)]
struct TestContentStore {
    store: Arc<RwLock<HashMap<ContentHash, SegmentId>>>,
}

impl ContentStore for TestContentStore {
    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        futures::executor::block_on(async { self.store.read().await.get(hash).copied() })
    }

    fn register_content(&self, hash: &ContentHash, segment_id: SegmentId) {
        futures::executor::block_on(async {
            self.store.write().await.insert(hash.clone(), segment_id);
        });
    }
}

struct DataMotionNode {
    agent: ScalingAgent<TestContentStore>,
    mesh: Arc<MeshNode<TestContentStore>>,
    catalog: Arc<InMemoryCatalog>,
    content_store: Arc<RwLock<TestContentStore>>,
    nvram: Arc<RwLock<NvramLog>>,
    addr: SocketAddr,
    _temp_dir: tempfile::TempDir,
}

impl DataMotionNode {
    async fn new(port: u16) -> Self {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir for nvram log");
        let log_path = temp_dir
            .path()
            .join(format!("data_motion_nvram_{}.log", port));

        let catalog = Arc::new(InMemoryCatalog::default());
        let content_store = Arc::new(RwLock::new(TestContentStore::default()));
        let nvram = Arc::new(RwLock::new(
            NvramLog::open(&log_path).expect("failed to open nvram log"),
        ));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));

        let zone = ZoneId::Metro {
            name: format!("zone-{}", port),
        };
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();

        let mesh = Arc::new(
            MeshNode::new(
                zone,
                addr,
                content_store.clone(),
                nvram.clone(),
                key_manager.clone(),
            )
            .await
            .expect("failed to create mesh node"),
        );

        mesh.start(vec![]).await.expect("failed to start mesh node");
        sleep(Duration::from_millis(20)).await;

        let agent = ScalingAgent::with_runtime(
            mesh.clone(),
            Policy::metro_sync(),
            catalog.clone(),
            nvram.clone(),
            key_manager.clone(),
        );

        Self {
            agent,
            mesh,
            catalog,
            content_store,
            nvram,
            addr,
            _temp_dir: temp_dir,
        }
    }

    async fn seed_capsule(
        &self,
        capsule_id: CapsuleId,
        payload: &[u8],
        policy: &Policy,
    ) -> SegmentId {
        let segment_id = self
            .catalog
            .allocate_segment()
            .expect("failed to allocate segment id");
        {
            let log = self.nvram.write().await;
            log.append(segment_id, payload)
                .expect("failed to append payload");
        }

        let mut stats = DedupStats::default();
        stats.add_segment(payload.len() as u64, false);
        self.catalog
            .create_capsule(
                capsule_id,
                payload.len() as u64,
                policy,
                vec![segment_id],
                &stats,
            )
            .expect("failed to create capsule");

        segment_id
    }
}

async fn wait_for_segment(log: &Arc<RwLock<NvramLog>>, segment_id: SegmentId) -> bool {
    for _ in 0..20 {
        if log.read().await.get_segment_metadata(segment_id).is_ok() {
            return true;
        }
        sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
async fn data_motion_copy_preserves_source() {
    let src = DataMotionNode::new(24010).await;
    let dst = DataMotionNode::new(24011).await;

    src.mesh.register_peer(dst.mesh.id(), dst.addr).await;

    let capsule_id = CapsuleId::new();
    let policy = Policy::metro_sync();
    let payload = b"payload-data-motion-copy";
    let segment_id = src.seed_capsule(capsule_id, payload, &policy).await;

    let moved = src
        .agent
        .execute_data_motion(
            capsule_id,
            vec![dst.mesh.id()],
            MotionMode::Copy,
            false,
            "test-copy",
        )
        .await
        .expect("data motion copy failed");
    assert_eq!(moved, 1);
    assert!(wait_for_segment(&dst.nvram, segment_id).await);

    let hash = ContentHash::from_bytes(blake3::hash(payload).as_bytes());
    let stored_segment = dst.content_store.read().await.lookup_content(&hash);
    assert_eq!(stored_segment, Some(segment_id));
    let dst_data = dst.nvram.read().await.read(segment_id).unwrap();
    assert_eq!(dst_data.len(), payload.len());

    let src_data = src.nvram.read().await.read(segment_id).unwrap();
    assert_eq!(src_data, payload);
}

#[tokio::test]
async fn data_motion_move_cleans_source() {
    let src = DataMotionNode::new(24012).await;
    let dst = DataMotionNode::new(24013).await;

    src.mesh.register_peer(dst.mesh.id(), dst.addr).await;

    let capsule_id = CapsuleId::new();
    let policy = Policy::metro_sync();
    let payload = b"payload-data-motion-move";
    let segment_id = src.seed_capsule(capsule_id, payload, &policy).await;

    let moved = src
        .agent
        .execute_data_motion(
            capsule_id,
            vec![dst.mesh.id()],
            MotionMode::Move,
            false,
            "test-move",
        )
        .await
        .expect("data motion move failed");
    assert_eq!(moved, 1);
    assert!(wait_for_segment(&dst.nvram, segment_id).await);

    let hash = ContentHash::from_bytes(blake3::hash(payload).as_bytes());
    let stored_segment = dst.content_store.read().await.lookup_content(&hash);
    assert_eq!(stored_segment, Some(segment_id));
    let dst_data = dst.nvram.read().await.read(segment_id).unwrap();
    assert_eq!(dst_data.len(), payload.len());

    assert!(src
        .nvram
        .read()
        .await
        .get_segment_metadata(segment_id)
        .is_err());
    assert!(src.catalog.lookup_capsule(capsule_id).is_err());
}

#![cfg(feature = "phase4")]

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry, RegistryTransformOps};
use common::podms::TransformOps;
use common::podms::ZoneId;
use common::{CapsuleId, ContentHash, Policy, SegmentId};
use encryption::keymanager::MASTER_KEY_SIZE;
use encryption::KeyManager;
use nvram_sim::NvramLog;
use protocol_nvme::project_nvme_view;
use scaling::{ContentStore, MeshNode};
use std::fs;
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

async fn build_mesh(
    zone: ZoneId,
    nvram: Arc<RwLock<NvramLog>>,
    key_manager: Arc<RwLock<KeyManager>>,
) -> MeshNode<DummyContentStore> {
    let content = Arc::new(RwLock::new(DummyContentStore));
    MeshNode::new(
        zone,
        "127.0.0.1:0".parse().unwrap(),
        content,
        nvram,
        key_manager,
    )
    .await
    .unwrap()
}

fn cleanup(path: &str) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{path}.segments"));
}

#[tokio::test]
async fn view_decrypts_stored_data() {
    let capsule_hint = CapsuleId::new();
    let base = std::env::temp_dir().join(format!("nvme-view-{}", capsule_hint.as_uuid()));
    let _ = fs::create_dir_all(&base);
    let meta_path = base.join("registry.db");
    let nvram_path = base.join("nvram.log");

    let registry = CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).unwrap();
    let nvram = NvramLog::open(nvram_path.to_string_lossy().as_ref()).unwrap();

    let registry_for_pipeline = registry.clone();
    let nvram_for_pipeline = nvram.clone();
    let nvram_for_transform = nvram.clone();
    let nvram_for_mesh = nvram.clone();

    let master_key = [0xABu8; MASTER_KEY_SIZE];
    {
        let mut guard = registry.key_manager().lock().unwrap();
        *guard = KeyManager::new(master_key);
    }

    let pipeline = WritePipeline::with_key_manager(
        registry_for_pipeline,
        nvram_for_pipeline,
        KeyManager::new(master_key),
    );

    let policy = Policy::encrypted();
    let capsule_id = pipeline
        .write_capsule_with_policy_async(b"Space, the final frontier...", &policy)
        .await
        .unwrap();

    let capsule = registry.lookup(capsule_id).unwrap();
    let seg_id = capsule.segments[0];
    let ciphertext = nvram.read(seg_id).unwrap();
    assert_ne!(
        ciphertext, b"Space, the final frontier...",
        "storage should retain encrypted bytes"
    );

    let transform = RegistryTransformOps::with_nvram(
        registry.key_manager().clone(),
        Arc::new(nvram_for_transform),
    );
    let plaintext = transform
        .decrypt(capsule_id, &ciphertext, &policy.encryption, seg_id)
        .unwrap();
    assert_eq!(plaintext, b"Space, the final frontier...");

    let mesh = build_mesh(
        ZoneId::Metro {
            name: "zone-1".into(),
        },
        Arc::new(RwLock::new(nvram_for_mesh)),
        Arc::new(RwLock::new(KeyManager::new(master_key))),
    )
    .await;

    let view = project_nvme_view(capsule_id, &policy, &mesh, &registry)
        .await
        .expect("project view");
    assert_eq!(view.nvme_target().namespaces().len(), 1);

    cleanup(meta_path.to_string_lossy().as_ref());
    cleanup(nvram_path.to_string_lossy().as_ref());
    let _ = fs::remove_dir_all(base);
}

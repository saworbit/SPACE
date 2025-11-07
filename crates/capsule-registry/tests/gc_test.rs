use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::Policy;
use nvram_sim::NvramLog;
use std::fs;

fn setup_paths(prefix: &str) -> (String, String) {
    let log_path = format!("{}_gc.log", prefix);
    let meta_path = format!("{}_gc.metadata", prefix);
    let _ = fs::remove_file(&log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(&meta_path);
    (log_path, meta_path)
}

#[test]
fn refcounts_increase_and_decrease_with_capsules() {
    std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");

    let (log_path, meta_path) = setup_paths("refcount");

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let registry_view = registry.clone();
    let nvram = NvramLog::open(&log_path).unwrap();
    let nvram_view = nvram.clone();

    let pipeline = WritePipeline::new(registry, nvram);

    let data = b"shared payload ".repeat(512);
    let policy = Policy::default();

    let capsule_one = pipeline.write_capsule_with_policy(&data, &policy).unwrap();
    let capsule_two = pipeline.write_capsule_with_policy(&data, &policy).unwrap();

    let capsule_meta = registry_view.lookup(capsule_one).unwrap();
    assert!(!capsule_meta.segments.is_empty());
    let shared_seg = capsule_meta.segments[0];

    let segment = nvram_view.get_segment_metadata(shared_seg).unwrap();
    assert_eq!(segment.ref_count, 2);
    assert!(segment.deduplicated);

    // Delete one capsule – refcount should drop but segment remains.
    pipeline.delete_capsule(capsule_one).unwrap();
    let segment = nvram_view.get_segment_metadata(shared_seg).unwrap();
    assert_eq!(segment.ref_count, 1);
    assert!(segment.deduplicated == false);

    // Delete the final capsule – segment metadata and content mapping should vanish.
    let segment_hash = segment.content_hash.clone().expect("segment hash present");
    pipeline.delete_capsule(capsule_two).unwrap();

    assert!(nvram_view.get_segment_metadata(shared_seg).is_err());
    assert!(registry_view.lookup_content(&segment_hash).is_none());

    drop(pipeline);
    let _ = fs::remove_file(&log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(&meta_path);

    std::env::remove_var("SPACE_DISABLE_MODULAR_PIPELINE");
}

#[test]
fn garbage_collect_reclaims_orphan_segments() {
    std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");

    let (log_path, meta_path) = setup_paths("gc_sweep");

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let registry_view = registry.clone();
    let nvram = NvramLog::open(&log_path).unwrap();
    let nvram_view = nvram.clone();

    let pipeline = WritePipeline::new(registry, nvram);
    let capsule_id = pipeline.write_capsule(b"temporary capsule").unwrap();

    let capsule = registry_view.lookup(capsule_id).unwrap();
    let seg_id = capsule.segments[0];

    // Simulate a crash between capsule deletion and GC by manually zeroing refcount.
    let mut segment = nvram_view.get_segment_metadata(seg_id).unwrap();
    segment.ref_count = 0;
    segment.deduplicated = false;
    nvram_view
        .update_segment_metadata(seg_id, segment.clone())
        .unwrap();

    // Drop capsule metadata to make segment orphaned.
    registry_view.delete_capsule(capsule_id).unwrap();

    let reclaimed = pipeline.garbage_collect().unwrap();
    assert_eq!(reclaimed, 1);
    assert!(nvram_view.get_segment_metadata(seg_id).is_err());
    if let Some(hash) = segment.content_hash {
        assert!(registry_view.lookup_content(&hash).is_none());
    }

    drop(pipeline);
    let _ = fs::remove_file(&log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(&meta_path);

    std::env::remove_var("SPACE_DISABLE_MODULAR_PIPELINE");
}

#[cfg(feature = "modular_pipeline")]
mod modular_pipeline_gc {
    use super::*;
    use capsule_registry::modular_pipeline::{
        DefaultPolicyEvaluator, KeyManagerKeyring, XtsEncryptor,
    };
    use common::{Policy, SegmentId};
    use compression::Lz4ZstdCompressor;
    use dedup::Blake3Deduper;
    use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
    use futures::executor::block_on;
    use pipeline::Pipeline as ModularPipeline;
    use storage::NvramBackend;
    use std::sync::{Arc, Mutex};

    #[test]
    fn modular_pipeline_handles_key_rotation() {
        std::env::remove_var("SPACE_DISABLE_MODULAR_PIPELINE");

        let log_path = "modular_gc.log";
        let segments_path = format!("{}.segments", log_path);
        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(&segments_path);

        let storage = NvramBackend::open(&log_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0x3Cu8; MASTER_KEY_SIZE])));

        let encryptor = XtsEncryptor::new(Arc::clone(&key_manager));
        let keyring = KeyManagerKeyring::new(Arc::clone(&key_manager));
        let mut pipeline = ModularPipeline::new(
            Lz4ZstdCompressor::default(),
            Blake3Deduper::default(),
            encryptor,
            storage.clone(),
            DefaultPolicyEvaluator::default(),
            Some(keyring),
            pipeline::InMemoryCatalog::default(),
        );

        let mut policy = Policy::encrypted();
        policy.dedupe = false;

        block_on(pipeline.write_capsule(b"modular gc data payload", &policy)).unwrap();

        {
            let mut km = key_manager.lock().unwrap();
            km.rotate().unwrap();
        }

        block_on(pipeline.write_capsule(b"modular gc data payload second", &policy)).unwrap();

        let log = nvram_sim::NvramLog::open(&log_path).unwrap();
        let first = log.get_segment_metadata(SegmentId(0)).unwrap();
        let second = log.get_segment_metadata(SegmentId(1)).unwrap();

        assert!(first.encrypted && second.encrypted);
        assert_ne!(first.key_version, second.key_version);

        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(&segments_path);

        std::env::remove_var("SPACE_DISABLE_MODULAR_PIPELINE");
    }
}

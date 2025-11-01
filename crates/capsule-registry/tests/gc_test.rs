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
}

#[test]
fn garbage_collect_reclaims_orphan_segments() {
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
}

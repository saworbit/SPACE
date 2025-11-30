#![cfg(feature = "pipeline_async")]

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::{Policy, SEGMENT_SIZE};
use nvram_sim::NvramLog;
use std::fs;
use std::path::Path;
use std::sync::Once;
use uuid::Uuid;

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Force tests to hit the native async pipeline even when the modular
        // pipeline feature is enabled so we exercise the NVMe/NVRAM path.
        std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");
    });
}

fn test_paths(prefix: &str) -> (String, String) {
    let base = std::env::temp_dir().join("space_async_pipeline_tests");
    let _ = fs::create_dir_all(&base);
    let unique = format!("{}_{}", prefix, Uuid::new_v4());
    let log = base.join(format!("{unique}.log"));
    let meta = base.join(format!("{unique}.metadata"));
    (
        log.to_string_lossy().to_string(),
        meta.to_string_lossy().to_string(),
    )
}

fn cleanup(path: &str) {
    cleanup_path(path);
    cleanup_path(&format!("{}.segments", path));
}

fn cleanup_path(path: &str) {
    let p = Path::new(path);
    match fs::remove_file(p) {
        Ok(_) => (),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            if err.kind() == std::io::ErrorKind::IsADirectory {
                let _ = fs::remove_dir_all(p);
            }
        }
    }
}

#[test]
fn async_pipeline_processes_segments_in_order() {
    init();
    let (log_path, meta_path) = test_paths("async_pipeline");
    cleanup(&log_path);
    cleanup(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).expect("open registry");
    let nvram = NvramLog::open(&log_path).expect("open nvram");
    let pipeline = WritePipeline::new(registry.clone(), nvram.clone());

    // Create data spanning multiple segments to exercise ordering.
    let segment_count = 3;
    let mut data = Vec::with_capacity(SEGMENT_SIZE * segment_count + 2048);
    for seg in 0..segment_count {
        data.extend(std::iter::repeat_n(seg as u8, SEGMENT_SIZE));
    }
    data.extend((0..2048).map(|i| ((i * 37) % 251) as u8));

    let policy = Policy::default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let capsule_id = rt
        .block_on(pipeline.write_capsule_with_policy_async(&data, &policy))
        .expect("write capsule");

    let roundtrip = pipeline.read_capsule(capsule_id).expect("read capsule");
    assert_eq!(data, roundtrip, "round-trip data mismatch");

    let reopened = CapsuleRegistry::open(&meta_path).expect("reopen registry");
    let capsule = reopened.lookup(capsule_id).expect("capsule lookup");

    let expected_segments = data.len().div_ceil(SEGMENT_SIZE);
    assert_eq!(
        capsule.segments.len(),
        expected_segments,
        "expected multi-segment capsule (tail partial segment included)"
    );

    // Ensure segments are recorded in ascending order.
    assert!(
        capsule
            .segments
            .windows(2)
            .all(|window| window[0].0 < window[1].0),
        "segment identifiers not strictly increasing"
    );

    cleanup(&log_path);
    cleanup(&meta_path);
}

#[test]
fn async_pipeline_deduplicates_repeated_payloads() {
    init();
    let (log_path, meta_path) = test_paths("async_pipeline_dedup");
    cleanup(&log_path);
    cleanup(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).expect("open registry");
    let nvram = NvramLog::open(&log_path).expect("open nvram");
    let pipeline = WritePipeline::new(registry.clone(), nvram.clone());

    let payload = b"SPACE async dedup".repeat(1024);
    let policy = Policy::default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let first_capsule = rt
        .block_on(pipeline.write_capsule_with_policy_async(&payload, &policy))
        .expect("first capsule");
    let second_capsule = rt
        .block_on(pipeline.write_capsule_with_policy_async(&payload, &policy))
        .expect("second capsule");

    let reopened = CapsuleRegistry::open(&meta_path).expect("reopen registry");
    let first = reopened.lookup(first_capsule).expect("first lookup");
    let second = reopened.lookup(second_capsule).expect("second lookup");

    assert_eq!(
        first.segments, second.segments,
        "deduplication should reuse the same segments"
    );

    cleanup(&log_path);
    cleanup(&meta_path);
}

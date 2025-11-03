#![cfg(feature = "pipeline_async")]

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::{Policy, SEGMENT_SIZE};
use nvram_sim::NvramLog;
use std::fs;

fn cleanup(log_path: &str, meta_path: &str) {
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn async_pipeline_processes_segments_in_order() {
    let log_path = "async_pipeline.log";
    let meta_path = "async_pipeline.metadata";
    cleanup(log_path, meta_path);

    let registry = CapsuleRegistry::open(meta_path).expect("open registry");
    let nvram = NvramLog::open(log_path).expect("open nvram");
    let pipeline = WritePipeline::new(registry.clone(), nvram.clone());

    // Create data spanning multiple segments to exercise ordering.
    let segment_count = 3;
    let mut data = Vec::with_capacity(SEGMENT_SIZE * segment_count + 2048);
    for seg in 0..segment_count {
        data.extend(std::iter::repeat(seg as u8).take(SEGMENT_SIZE));
    }
    data.extend((0..2048).map(|i| ((i * 37) % 251) as u8));

    let policy = Policy::default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let capsule_id = rt
        .block_on(pipeline.write_capsule_with_policy_async(&data, &policy))
        .expect("write capsule");

    let roundtrip = pipeline.read_capsule(capsule_id).expect("read capsule");
    assert_eq!(data, roundtrip, "round-trip data mismatch");

    let reopened = CapsuleRegistry::open(meta_path).expect("reopen registry");
    let capsule = reopened.lookup(capsule_id).expect("capsule lookup");

    let expected_segments = (data.len() + SEGMENT_SIZE - 1) / SEGMENT_SIZE;
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

    cleanup(log_path, meta_path);
}

#[test]
fn async_pipeline_deduplicates_repeated_payloads() {
    let log_path = "async_pipeline_dedup.log";
    let meta_path = "async_pipeline_dedup.metadata";
    cleanup(log_path, meta_path);

    let registry = CapsuleRegistry::open(meta_path).expect("open registry");
    let nvram = NvramLog::open(log_path).expect("open nvram");
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

    let reopened = CapsuleRegistry::open(meta_path).expect("reopen registry");
    let first = reopened.lookup(first_capsule).expect("first lookup");
    let second = reopened.lookup(second_capsule).expect("second lookup");

    assert_eq!(
        first.segments, second.segments,
        "deduplication should reuse the same segments"
    );

    cleanup(log_path, meta_path);
}

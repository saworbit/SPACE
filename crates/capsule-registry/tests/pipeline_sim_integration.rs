//! Integration tests for pipeline with NVRAM simulation.
//!
//! These tests validate that the write/read pipeline correctly integrates
//! with the sim-nvram crate, enabling end-to-end testing of compression,
//! deduplication, and encryption without physical hardware.

use anyhow::Result;
use common::*;
use sim_nvram::start_nvram_sim;
use std::fs;

/// Test basic write/read cycle with simulated NVRAM.
#[test]
fn test_pipeline_with_nvram_sim() -> Result<()> {
    let test_path = "test_pipeline_sim.log";

    // Start NVRAM simulation
    let log = start_nvram_sim(test_path)?;

    // Write test data
    let segment_id = SegmentId(100);
    let test_data = b"Integration test data for pipeline simulation";

    let segment = log.append(segment_id, test_data)?;
    assert_eq!(segment.len, test_data.len() as u32);

    // Read back
    let read_data = log.read(segment_id)?;
    assert_eq!(read_data, test_data);

    // Cleanup
    cleanup_test_files(test_path);
    Ok(())
}

/// Test transaction support with simulation.
#[test]
fn test_pipeline_transaction_with_sim() -> Result<()> {
    let test_path = "test_pipeline_tx_sim.log";

    let log = start_nvram_sim(test_path)?;
    let mut tx = log.begin_transaction()?;

    // Append multiple segments in transaction
    let seg1 = SegmentId(101);
    let seg2 = SegmentId(102);

    tx.append_segment(seg1, b"segment 1 data")?;
    tx.append_segment(seg2, b"segment 2 data")?;

    // Commit transaction
    tx.commit()?;

    // Verify both segments are readable
    let data1 = log.read(seg1)?;
    let data2 = log.read(seg2)?;

    assert_eq!(data1, b"segment 1 data");
    assert_eq!(data2, b"segment 2 data");

    cleanup_test_files(test_path);
    Ok(())
}

/// Test deduplication with simulated storage.
#[test]
fn test_dedup_with_nvram_sim() -> Result<()> {
    let test_path = "test_dedup_sim.log";

    let log = start_nvram_sim(test_path)?;

    // Write same data twice (simulating dedup scenario)
    let seg1 = SegmentId(103);
    let seg2 = SegmentId(104);
    let data = b"duplicate data content";

    log.append(seg1, data)?;
    log.append(seg2, data)?;

    // In real pipeline with dedup enabled, seg2 would reference seg1
    // Here we just verify both writes succeed
    let read1 = log.read(seg1)?;
    let read2 = log.read(seg2)?;

    assert_eq!(read1, data);
    assert_eq!(read2, data);

    cleanup_test_files(test_path);
    Ok(())
}

/// Test refcount operations (dedup support).
#[test]
fn test_refcount_with_sim() -> Result<()> {
    let test_path = "test_refcount_sim.log";

    let log = start_nvram_sim(test_path)?;
    let seg_id = SegmentId(105);

    // Initial write (ref_count = 1)
    let segment = log.append(seg_id, b"refcounted data")?;
    assert_eq!(segment.ref_count, 1);

    // Increment refcount (simulate dedup)
    let updated = log.increment_refcount(seg_id)?;
    assert_eq!(updated.ref_count, 2);
    assert!(updated.deduplicated);

    // Decrement
    let updated = log.decrement_refcount(seg_id)?;
    assert_eq!(updated.ref_count, 1);
    assert!(!updated.deduplicated);

    cleanup_test_files(test_path);
    Ok(())
}

/// Test encryption metadata with simulation.
///
/// Note: This tests metadata storage, not actual encryption (which is
/// handled by the encryption crate).
#[test]
fn test_encryption_metadata_with_sim() -> Result<()> {
    let test_path = "test_encryption_sim.log";

    let log = start_nvram_sim(test_path)?;
    let seg_id = SegmentId(106);

    // Write segment
    let mut segment = log.append(seg_id, b"encrypted data placeholder")?;

    // Update with encryption metadata
    segment.encrypted = true;
    segment.encryption_version = Some(1);
    segment.key_version = Some(42);
    segment.tweak_nonce = Some([0u8; 16]);
    segment.integrity_tag = Some([0u8; 16]);

    log.update_segment_metadata(seg_id, segment.clone())?;

    // Read back metadata
    let retrieved = log.get_segment_metadata(seg_id)?;
    assert!(retrieved.encrypted);
    assert_eq!(retrieved.encryption_version, Some(1));
    assert_eq!(retrieved.key_version, Some(42));

    cleanup_test_files(test_path);
    Ok(())
}

/// Helper to clean up test files.
fn cleanup_test_files(path: &str) {
    fs::remove_file(path).ok();
    fs::remove_file(format!("{}.segments", path)).ok();
}

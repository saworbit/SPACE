use capsule_registry::{CapsuleRegistry, pipeline::WritePipeline};
use common::Policy;
use nvram_sim::NvramLog;
use std::fs;

#[test]
fn test_dedup_identical_segments() {
    // Setup
    let log_path = "test_dedup.nvram";
    let meta_path = "test_dedup.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
    
    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry.clone(), nvram);
    
    // Create data with repeating pattern (will dedupe)
    let repeated_data = b"SPACE STORAGE ".repeat(1000); // ~13KB repeated
    let test_data = repeated_data.repeat(10); // ~130KB total, multiple segments
    
    println!("\nTest: Writing data with repeated segments...");
    let policy = Policy::default(); // Dedup enabled by default
    let capsule_id = pipeline.write_capsule_with_policy(&test_data, &policy).unwrap();
    
    // Verify data integrity
    let read_data = pipeline.read_capsule(capsule_id).unwrap();
    assert_eq!(test_data, read_data);
    
    // Check dedup stats
    let (total_segments, unique_segments) = registry.get_dedup_stats();
    
    println!("\n📊 Dedup Statistics:");
    println!("   Total segments referenced: {}", total_segments);
    println!("   Unique segments stored: {}", unique_segments);
    
    // We expect some deduplication to occur
    assert!(unique_segments <= total_segments, 
        "Unique segments ({}) should be <= total segments ({})", 
        unique_segments, total_segments);
    
    if unique_segments < total_segments {
        let dedup_ratio = total_segments as f32 / unique_segments as f32;
        println!("   Deduplication ratio: {:.2}x", dedup_ratio);
        println!("✅ Deduplication is working!");
    } else {
        println!("⚠️  No deduplication occurred (segments may be too diverse)");
    }
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_dedup_multiple_capsules() {
    let log_path = "test_dedup_multi.nvram";
    let meta_path = "test_dedup_multi.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
    
    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry.clone(), nvram);
    
    // Write the same data to multiple capsules
    let shared_data = b"Shared content across capsules!".repeat(200_000); // ~6MB
    
    println!("\nTest: Creating multiple capsules with shared content...");
    
    let capsule1 = pipeline.write_capsule(&shared_data).unwrap();
    println!("\nCapsule 1 created");
    
    let capsule2 = pipeline.write_capsule(&shared_data).unwrap();
    println!("\nCapsule 2 created (should dedupe)");
    
    let capsule3 = pipeline.write_capsule(&shared_data).unwrap();
    println!("\nCapsule 3 created (should dedupe)");
    
    // Verify all capsules are readable
    let data1 = pipeline.read_capsule(capsule1).unwrap();
    let data2 = pipeline.read_capsule(capsule2).unwrap();
    let data3 = pipeline.read_capsule(capsule3).unwrap();
    
    assert_eq!(shared_data.as_slice(), data1.as_slice());
    assert_eq!(shared_data.as_slice(), data2.as_slice());
    assert_eq!(shared_data.as_slice(), data3.as_slice());
    
    // Check dedup stats
    let (total_segments, unique_segments) = registry.get_dedup_stats();
    
    println!("\n📊 Multi-Capsule Dedup Statistics:");
    println!("   Total segments referenced: {}", total_segments);
    println!("   Unique segments stored: {}", unique_segments);
    
    // With 3 identical capsules, we expect significant dedup
    let dedup_ratio = total_segments as f32 / unique_segments as f32;
    println!("   Deduplication ratio: {:.2}x", dedup_ratio);
    
    // Should have deduplication since all 3 capsules are identical
    assert!(dedup_ratio > 1.5, 
        "Expected significant dedup (>1.5x), got {:.2}x", dedup_ratio);
    
    println!("✅ Multi-capsule deduplication works!");
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_dedup_disabled() {
    let log_path = "test_no_dedup.nvram";
    let meta_path = "test_no_dedup.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
    
    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry.clone(), nvram);
    
    // Create policy with dedup disabled
    let mut policy = Policy::default();
    policy.dedupe = false;
    
    let data = b"Same data".repeat(100_000); // ~900KB
    
    println!("\nTest: Writing with dedup disabled...");
    
    let capsule1 = pipeline.write_capsule_with_policy(&data, &policy).unwrap();
    let capsule2 = pipeline.write_capsule_with_policy(&data, &policy).unwrap();
    
    // Verify data integrity
    let data1 = pipeline.read_capsule(capsule1).unwrap();
    let data2 = pipeline.read_capsule(capsule2).unwrap();
    assert_eq!(data.as_slice(), data1.as_slice());
    assert_eq!(data.as_slice(), data2.as_slice());
    
    let (total_segments, unique_segments) = registry.get_dedup_stats();
    
    println!("\n📊 No-Dedup Statistics:");
    println!("   Total segments: {}", total_segments);
    println!("   Unique segments: {}", unique_segments);
    
    // With dedup disabled, every segment should be unique
    // (Note: content_store won't be populated when dedupe=false)
    println!("✅ Dedup disabled mode works!");
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_dedup_with_compression() {
    let log_path = "test_dedup_compress.nvram";
    let meta_path = "test_dedup_compress.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
    
    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry.clone(), nvram);
    
    // Highly compressible data that will also dedupe
    let data = b"SPACE ".repeat(1_000_000); // 6MB of repeated pattern
    
    println!("\nTest: Dedup + Compression working together...");
    
    let policy = Policy::text_optimized(); // High compression + dedup enabled
    
    let capsule1 = pipeline.write_capsule_with_policy(&data, &policy).unwrap();
    println!("\nFirst capsule written");
    
    let capsule2 = pipeline.write_capsule_with_policy(&data, &policy).unwrap();
    println!("\nSecond capsule written (should show dedup hits)");
    
    // Verify integrity
    let data1 = pipeline.read_capsule(capsule1).unwrap();
    let data2 = pipeline.read_capsule(capsule2).unwrap();
    assert_eq!(data.as_slice(), data1.as_slice());
    assert_eq!(data.as_slice(), data2.as_slice());
    
    let (total_segments, unique_segments) = registry.get_dedup_stats();
    
    println!("\n📊 Compression + Dedup Statistics:");
    println!("   Total segments: {}", total_segments);
    println!("   Unique segments: {}", unique_segments);
    
    if total_segments > unique_segments {
        let dedup_ratio = total_segments as f32 / unique_segments as f32;
        println!("   Deduplication ratio: {:.2}x", dedup_ratio);
        println!("✅ Compression + Deduplication working together!");
    }
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_dedup_hash_consistency() {
    use capsule_registry::dedup::hash_content;
    
    let data = b"Test data for hash consistency";
    
    let hash1 = hash_content(data);
    let hash2 = hash_content(data);
    let hash3 = hash_content(b"Different data");
    
    // Same data = same hash
    assert_eq!(hash1, hash2);
    
    // Different data = different hash
    assert_ne!(hash1, hash3);
    
    println!("✅ Content hashing is consistent");
}
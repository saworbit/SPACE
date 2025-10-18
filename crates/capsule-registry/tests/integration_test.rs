use capsule_registry::{CapsuleRegistry, pipeline::WritePipeline};
use nvram_sim::NvramLog;
use std::fs;

#[test]
fn test_write_and_read_capsule() {
    // Setup
    let log_path = "test_nvram.log";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    
    let registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);
    
    // Write test data
    let test_data = b"Hello SPACE! This is capsule test data.";
    let capsule_id = pipeline.write_capsule(test_data).unwrap();
    
    println!("Created capsule: {:?}", capsule_id);
    
    // Read it back
    let read_data = pipeline.read_capsule(capsule_id).unwrap();
    
    assert_eq!(test_data.as_slice(), read_data.as_slice());
    println!("✅ Write/Read test passed!");
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
}

#[test]
fn test_compression_integration() {
    let log_path = "test_compression.log";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    
    let registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);
    
    // Create highly compressible data
    let test_data = b"SPACE ".repeat(10000); // 60KB
    
    let capsule_id = pipeline.write_capsule(&test_data).unwrap();
    let read_data = pipeline.read_capsule(capsule_id).unwrap();
    
    assert_eq!(test_data, read_data);
    println!("✅ Compression integration test passed!");
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
}
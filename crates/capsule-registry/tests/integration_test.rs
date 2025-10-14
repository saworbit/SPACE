use capsule_registry::{CapsuleRegistry, pipeline::WritePipeline};
use nvram_sim::NvramLog;
use std::fs;

#[test]
fn test_write_and_read_capsule() {
    // Setup
    let log_path = "test_nvram.log";
    let _ = fs::remove_file(log_path); // Clean up from previous runs
    
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
    fs::remove_file(log_path).unwrap();
}
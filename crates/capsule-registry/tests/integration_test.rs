use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use nvram_sim::NvramLog;
use std::fs;

#[test]
fn test_write_and_read_capsule() {
    let log_path = "test_nvram.log";
    let meta_path = "test_nvram.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"Hello SPACE! This is capsule test data.";
    let capsule_id = pipeline.write_capsule(test_data).unwrap();

    println!("Created capsule: {:?}", capsule_id);

    let read_data = pipeline.read_capsule(capsule_id).unwrap();
    assert_eq!(test_data.as_slice(), read_data.as_slice());
    println!("Write/Read test passed!");

    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_compression_integration() {
    let log_path = "test_compression.log";
    let meta_path = "test_compression.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"SPACE ".repeat(10_000);
    let capsule_id = pipeline.write_capsule(&test_data).unwrap();
    let read_data = pipeline.read_capsule(capsule_id).unwrap();

    assert_eq!(test_data, read_data);
    println!("Compression integration test passed!");

    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

use capsule_registry::CapsuleRegistry;
use nvram_sim::NvramLog;
use protocol_s3::S3View;
use std::fs;

#[tokio::test]
async fn test_s3_put_and_get() {
    // Setup
    let log_path = "test_s3.nvram";
    let meta_path = "test_s3.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Test data
    let test_data = b"Hello from S3 view! This is capsule storage.".to_vec();

    // PUT object
    let capsule_id = s3
        .put_object("test-bucket", "hello.txt", test_data.clone())
        .await
        .unwrap();
    println!("✅ PUT: Created capsule {:?}", capsule_id);

    // GET object
    let retrieved = s3.get_object("test-bucket", "hello.txt").await.unwrap();
    assert_eq!(retrieved, test_data);
    println!("✅ GET: Retrieved {} bytes", retrieved.len());

    // HEAD object
    let metadata = s3.head_object("test-bucket", "hello.txt").unwrap();
    assert_eq!(metadata.size(), test_data.len() as u64);
    assert_eq!(metadata.content_type(), "text/plain");
    assert_eq!(metadata.capsule_id(), capsule_id);
    println!("✅ HEAD: Verified metadata");

    // LIST objects
    let objects = s3.list_objects("test-bucket").unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].key(), "test-bucket/hello.txt");
    println!("✅ LIST: Found {} objects", objects.len());

    // DELETE object
    s3.delete_object("test-bucket", "hello.txt").unwrap();
    let result = s3.get_object("test-bucket", "hello.txt").await;
    assert!(result.is_err());
    println!("✅ DELETE: Object removed from key map");

    // Cleanup
    fs::remove_file(log_path).unwrap();
    fs::remove_file(format!("{}.segments", log_path)).unwrap();
    fs::remove_file(meta_path).unwrap();

    println!("\n🎉 All S3 view tests passed!");
}

#[tokio::test]
async fn test_s3_multiple_objects() {
    let log_path = "test_s3_multi.nvram";
    let meta_path = "test_s3_multi.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Create multiple objects
    s3.put_object("bucket1", "file1.txt", b"Content 1".to_vec())
        .await
        .unwrap();
    s3.put_object("bucket1", "file2.txt", b"Content 2".to_vec())
        .await
        .unwrap();
    s3.put_object("bucket2", "file3.txt", b"Content 3".to_vec())
        .await
        .unwrap();

    // List bucket1
    let bucket1_objects = s3.list_objects("bucket1").unwrap();
    assert_eq!(bucket1_objects.len(), 2);
    println!("✅ Bucket1 has {} objects", bucket1_objects.len());

    // List bucket2
    let bucket2_objects = s3.list_objects("bucket2").unwrap();
    assert_eq!(bucket2_objects.len(), 1);
    println!("✅ Bucket2 has {} objects", bucket2_objects.len());

    // Verify content
    let data = s3.get_object("bucket1", "file2.txt").await.unwrap();
    assert_eq!(data, b"Content 2");

    // Cleanup
    fs::remove_file(log_path).unwrap();
    fs::remove_file(format!("{}.segments", log_path)).unwrap();
    fs::remove_file(meta_path).unwrap();

    println!("🎉 Multi-object test passed!");
}

#[tokio::test]
async fn test_s3_large_object() {
    let log_path = "test_s3_large.nvram";
    let meta_path = "test_s3_large.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Create 10MB object (will span multiple 4MB segments)
    let large_data: Vec<u8> = (0..10_000_000).map(|i| (i % 256) as u8).collect();

    println!("Creating large object: {} bytes", large_data.len());

    s3.put_object("test", "large.bin", large_data.clone())
        .await
        .unwrap();
    println!("✅ PUT: Stored large object");

    let retrieved = s3.get_object("test", "large.bin").await.unwrap();
    assert_eq!(retrieved.len(), large_data.len());
    assert_eq!(retrieved, large_data);
    println!("✅ GET: Retrieved and verified {} bytes", retrieved.len());

    // Cleanup
    fs::remove_file(log_path).unwrap();
    fs::remove_file(format!("{}.segments", log_path)).unwrap();
    fs::remove_file(meta_path).unwrap();

    println!("🎉 Large object test passed!");
}

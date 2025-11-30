use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::{stream, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::S3View;
use std::{fs, path::Path, sync::Once};
use uuid::Uuid;

fn init_native_pipeline() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");
    });
}

#[tokio::test]
async fn test_s3_put_and_get() {
    init_native_pipeline();
    // Setup
    let (log_path, meta_path) = temp_paths("test_s3");
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Test data
    let test_data = b"Hello from S3 view! This is capsule storage.".to_vec();

    // PUT object
    let capsule_id = s3
        .put_object(
            "test-bucket",
            "hello.txt",
            stream_from_vec(test_data.clone()),
        )
        .await
        .unwrap();
    println!("PUT: Created capsule {:?}", capsule_id);

    // GET object
    let retrieved_stream = s3.get_object("test-bucket", "hello.txt").await.unwrap();
    let retrieved = collect_stream(retrieved_stream).await;
    assert_eq!(retrieved, test_data);
    println!("GET: Retrieved {} bytes", retrieved.len());

    // HEAD object
    let metadata = s3.head_object("test-bucket", "hello.txt").unwrap();
    assert_eq!(metadata.size(), test_data.len() as u64);
    assert_eq!(metadata.content_type(), "text/plain");
    assert_eq!(metadata.capsule_id(), capsule_id);
    println!("HEAD: Verified metadata");

    // LIST objects
    let objects = s3.list_objects("test-bucket").unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].key(), "test-bucket/hello.txt");
    println!("LIST: Found {} objects", objects.len());

    // DELETE object
    s3.delete_object("test-bucket", "hello.txt").unwrap();
    let result = s3.get_object("test-bucket", "hello.txt").await;
    assert!(result.is_err());
    println!("DELETE: Object removed from key map");

    // Cleanup
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    println!("\nAll S3 view tests passed!");
}

#[tokio::test]
async fn test_s3_multiple_objects() {
    init_native_pipeline();
    let (log_path, meta_path) = temp_paths("test_s3_multi");
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Create multiple objects
    s3.put_object(
        "bucket1",
        "file1.txt",
        stream_from_vec(b"Content 1".to_vec()),
    )
    .await
    .unwrap();
    s3.put_object(
        "bucket1",
        "file2.txt",
        stream_from_vec(b"Content 2".to_vec()),
    )
    .await
    .unwrap();
    s3.put_object(
        "bucket2",
        "file3.txt",
        stream_from_vec(b"Content 3".to_vec()),
    )
    .await
    .unwrap();

    // List bucket1
    let bucket1_objects = s3.list_objects("bucket1").unwrap();
    assert_eq!(bucket1_objects.len(), 2);
    println!("Bucket1 has {} objects", bucket1_objects.len());

    // List bucket2
    let bucket2_objects = s3.list_objects("bucket2").unwrap();
    assert_eq!(bucket2_objects.len(), 1);
    println!("Bucket2 has {} objects", bucket2_objects.len());

    // Verify content
    let data_stream = s3.get_object("bucket1", "file2.txt").await.unwrap();
    let data = collect_stream(data_stream).await;
    assert_eq!(data, b"Content 2");

    // Cleanup
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    println!("Multi-object test passed!");
}

#[tokio::test]
async fn test_s3_large_object() {
    init_native_pipeline();
    let (log_path, meta_path) = temp_paths("test_s3_large");
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let s3 = S3View::new(registry, nvram);

    // Create 10MB object (will span multiple 4MB segments)
    let large_data = generate_large_payload(10_000_000);

    println!("Creating large object: {} bytes", large_data.len());

    s3.put_object("test", "large.bin", stream_from_vec(large_data.clone()))
        .await
        .unwrap();
    println!("PUT: Stored large object");

    let retrieved_stream = s3.get_object("test", "large.bin").await.unwrap();
    let retrieved = collect_stream(retrieved_stream).await;
    assert_eq!(retrieved.len(), large_data.len());
    assert_eq!(retrieved, large_data);
    println!("GET: Retrieved and verified {} bytes", retrieved.len());

    // Cleanup
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    println!("Large object test passed!");
}

fn stream_from_vec(
    data: Vec<u8>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream::once(async move { Ok(Bytes::from(data)) })
}

async fn collect_stream<S, E>(stream: S) -> Vec<u8>
where
    S: futures::Stream<Item = Result<Bytes, E>>,
    E: std::fmt::Display + std::fmt::Debug,
{
    stream
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .expect("streaming read failed")
}

fn temp_paths(prefix: &str) -> (String, String) {
    let base = std::env::temp_dir().join("space_s3_tests");
    let _ = fs::create_dir_all(&base);
    let unique = format!("{}_{}", prefix, Uuid::new_v4());
    let log = base.join(format!("{unique}.nvram"));
    let meta = base.join(format!("{unique}.metadata"));
    (
        log.to_string_lossy().to_string(),
        meta.to_string_lossy().to_string(),
    )
}

fn cleanup_paths(path: &str) {
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

fn generate_large_payload(len: usize) -> Vec<u8> {
    // Simple LCG to avoid identical segments triggering dedup-only code paths in the registry.
    let mut seed: u32 = 0x1234_5678;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((seed >> 24) as u8);
    }
    out
}

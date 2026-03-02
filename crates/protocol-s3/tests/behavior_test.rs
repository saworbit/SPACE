//! Behavior-driven scenario tests for the S3 protocol view.
//!
//! Each test follows Given/When/Then structure to describe user-facing
//! behavior rather than implementation details, adapted for Rust's
//! native test framework.
//!
//! Naming convention: `scenario_<given>_<when>_<then>`

use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::{stream, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::S3View;
use std::{fs, path::Path, sync::Once};
use uuid::Uuid;

// ── Test infrastructure ─────────────────────────────────────────

fn init_native_pipeline() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");
    });
}

struct TestHarness {
    s3: S3View,
    log_path: String,
    meta_path: String,
}

impl TestHarness {
    fn new(prefix: &str) -> Self {
        init_native_pipeline();
        let base = std::env::temp_dir().join("space_behavior_tests");
        let _ = fs::create_dir_all(&base);
        let unique = format!("{}_{}", prefix, Uuid::new_v4());
        let log_path = base
            .join(format!("{unique}.nvram"))
            .to_string_lossy()
            .to_string();
        let meta_path = base
            .join(format!("{unique}.metadata"))
            .to_string_lossy()
            .to_string();
        cleanup_paths(&log_path);
        cleanup_paths(&meta_path);

        let registry = CapsuleRegistry::open(&meta_path).unwrap();
        let nvram = NvramLog::open(&log_path).unwrap();
        let s3 = S3View::new(registry, nvram);

        Self {
            s3,
            log_path,
            meta_path,
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        cleanup_paths(&self.log_path);
        cleanup_paths(&self.meta_path);
    }
}

// ── Scenarios ───────────────────────────────────────────────────

/// Scenario: A user uploads an object and retrieves it unchanged.
///
/// Given an empty bucket
/// When the user PUTs a text object
/// Then GET returns the exact same bytes
/// And HEAD reports the correct size and content type
#[tokio::test]
async fn scenario_empty_bucket_put_object_retrieved_unchanged() {
    let h = TestHarness::new("roundtrip");
    let data = b"The quick brown fox jumps over the lazy dog.".to_vec();

    // When: PUT
    let id =
        h.s3.put_object("docs", "greeting.txt", stream_from_vec(data.clone()))
            .await
            .unwrap();

    // Then: GET returns exact bytes
    let got = collect_stream(h.s3.get_object("docs", "greeting.txt").await.unwrap()).await;
    assert_eq!(got, data, "GET must return the original payload unchanged");

    // And: HEAD reports correct metadata
    let meta = h.s3.head_object("docs", "greeting.txt").unwrap();
    assert_eq!(meta.size(), data.len() as u64);
    assert_eq!(meta.content_type(), "text/plain");
    assert_eq!(meta.capsule_id(), id);
}

/// Scenario: Deleting an object makes it inaccessible.
///
/// Given a bucket with one object
/// When the user DELETEs that object
/// Then GET for the same key returns an error
/// And LIST shows an empty bucket
#[tokio::test]
async fn scenario_existing_object_delete_becomes_inaccessible() {
    let h = TestHarness::new("delete");

    // Given: one object exists
    h.s3.put_object("tmp", "ephemeral.bin", stream_from_vec(b"data".to_vec()))
        .await
        .unwrap();

    // When: DELETE
    h.s3.delete_object("tmp", "ephemeral.bin").unwrap();

    // Then: GET fails
    let result = h.s3.get_object("tmp", "ephemeral.bin").await;
    assert!(result.is_err(), "GET after DELETE must fail");

    // And: LIST is empty
    let list = h.s3.list_objects("tmp").unwrap();
    assert!(
        list.is_empty(),
        "bucket must be empty after last object deleted"
    );
}

/// Scenario: Objects in different buckets are isolated.
///
/// Given two buckets each with objects
/// When the user lists each bucket
/// Then each bucket only contains its own objects
#[tokio::test]
async fn scenario_multi_bucket_lists_are_isolated() {
    let h = TestHarness::new("isolation");

    // Given: objects in two buckets
    h.s3.put_object("alpha", "a1.txt", stream_from_vec(b"A1".to_vec()))
        .await
        .unwrap();
    h.s3.put_object("alpha", "a2.txt", stream_from_vec(b"A2".to_vec()))
        .await
        .unwrap();
    h.s3.put_object("beta", "b1.txt", stream_from_vec(b"B1".to_vec()))
        .await
        .unwrap();

    // When/Then: each bucket lists only its own objects
    let alpha = h.s3.list_objects("alpha").unwrap();
    let beta = h.s3.list_objects("beta").unwrap();

    assert_eq!(alpha.len(), 2, "alpha bucket must have exactly 2 objects");
    assert_eq!(beta.len(), 1, "beta bucket must have exactly 1 object");
}

/// Scenario: Overwriting an object replaces its content.
///
/// Given a bucket with an existing object
/// When the user PUTs a new payload to the same key
/// Then GET returns the new content, not the old
#[tokio::test]
async fn scenario_overwrite_replaces_content() {
    let h = TestHarness::new("overwrite");
    let original = b"version 1".to_vec();
    let updated = b"version 2".to_vec();

    // Given: object exists
    h.s3.put_object("repo", "config.toml", stream_from_vec(original))
        .await
        .unwrap();

    // When: overwrite with new content
    h.s3.put_object("repo", "config.toml", stream_from_vec(updated.clone()))
        .await
        .unwrap();

    // Then: GET returns the updated content
    let got = collect_stream(h.s3.get_object("repo", "config.toml").await.unwrap()).await;
    assert_eq!(
        got, updated,
        "GET after overwrite must return the new payload"
    );
}

// ── Helpers ─────────────────────────────────────────────────────

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

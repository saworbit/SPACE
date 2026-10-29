//! Keys survive a restart (#85), and replaced or deleted objects free their
//! capsules (#86).

use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::{stream, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::{ListQuery, S3Error, S3View};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("s3_lifecycle_{name}_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(dir: &Path) -> (CapsuleRegistry, S3View) {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let view = S3View::new(
        registry.clone(),
        NvramLog::open(dir.join("space.nvram")).unwrap(),
    )
    .unwrap();
    (registry, view)
}

fn body(data: &[u8]) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream::once(futures::future::ready(Ok(Bytes::copy_from_slice(data))))
}

async fn read(view: &S3View, bucket: &str, key: &str) -> Vec<u8> {
    let object = view.get_object(bucket, key).await.unwrap();
    object
        .body
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap()
}

fn keys(view: &S3View, bucket: &str) -> Vec<String> {
    view.list_objects(bucket, &ListQuery::default())
        .unwrap()
        .objects
        .into_iter()
        .map(|o| o.key)
        .collect()
}

#[tokio::test]
async fn keys_and_buckets_survive_a_restart() {
    let dir = scratch("restart");
    let (registry, view) = open(&dir);
    view.put_object("demo", "hello.txt", body(b"Hello SPACE!"))
        .await
        .unwrap();
    view.create_bucket("empty").unwrap();
    drop((registry, view));

    let (_registry, view) = open(&dir);
    assert_eq!(read(&view, "demo", "hello.txt").await, b"Hello SPACE!");
    assert_eq!(keys(&view, "demo"), ["hello.txt"]);
    view.head_bucket("empty").unwrap();
    assert!(matches!(
        view.head_bucket("never"),
        Err(S3Error::NoSuchBucket(_))
    ));

    drop(view);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn overwrite_and_delete_free_the_old_capsule() {
    let dir = scratch("free");
    let (registry, view) = open(&dir);

    let first = view
        .put_object("b", "k", body(b"version one"))
        .await
        .unwrap();
    let second = view
        .put_object("b", "k", body(b"version two"))
        .await
        .unwrap();
    assert!(registry.get_capsule(first).unwrap().is_none());
    assert_eq!(registry.capsules().len(), 1);
    assert_eq!(registry.content_entries().len(), 1);
    assert_eq!(read(&view, "b", "k").await, b"version two");

    view.delete_object("b", "k").await.unwrap();
    assert!(registry.get_capsule(second).unwrap().is_none());
    assert!(registry.capsules().is_empty());
    assert!(registry.content_entries().is_empty());

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn overwrite_with_the_same_bytes_keeps_the_shared_segment() {
    let dir = scratch("same");
    let (registry, view) = open(&dir);

    view.put_object("b", "k", body(b"same bytes"))
        .await
        .unwrap();
    view.put_object("b", "k", body(b"same bytes"))
        .await
        .unwrap();
    assert_eq!(registry.capsules().len(), 1);
    assert_eq!(read(&view, "b", "k").await, b"same bytes");

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_key_whose_capsule_is_gone_reads_as_missing() {
    let dir = scratch("dangling");
    let (registry, view) = open(&dir);

    let id = view.put_object("b", "k", body(b"data")).await.unwrap();
    // What `spacectl delete` leaves behind for the S3 view.
    registry.delete_capsule(id).unwrap();

    assert!(matches!(
        view.head_object("b", "k"),
        Err(S3Error::NoSuchKey(_))
    ));
    assert!(keys(&view, "b").is_empty());
    view.put_object("b", "k", body(b"again")).await.unwrap();
    assert_eq!(read(&view, "b", "k").await, b"again");

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

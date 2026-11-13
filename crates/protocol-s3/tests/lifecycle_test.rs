//! Keys survive a restart (#85) and a lost index, an overwrite is all or
//! nothing, and replaced or deleted objects free their capsules (#86).

use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::{stream, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::{ListQuery, Preconditions, S3Error, S3View};
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
    view.create_bucket("demo").unwrap();
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
    view.create_bucket("bkt").unwrap();

    let first = view
        .put_object("bkt", "k", body(b"version one"))
        .await
        .unwrap();
    let second = view
        .put_object("bkt", "k", body(b"version two"))
        .await
        .unwrap();
    assert!(registry.get_capsule(first).unwrap().is_none());
    assert_eq!(registry.capsules().len(), 1);
    assert_eq!(registry.content_entries().len(), 1);
    assert_eq!(read(&view, "bkt", "k").await, b"version two");

    view.delete_object("bkt", "k").await.unwrap();
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
    view.create_bucket("bkt").unwrap();

    view.put_object("bkt", "k", body(b"same bytes"))
        .await
        .unwrap();
    view.put_object("bkt", "k", body(b"same bytes"))
        .await
        .unwrap();
    assert_eq!(registry.capsules().len(), 1);
    assert_eq!(read(&view, "bkt", "k").await, b"same bytes");

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_key_whose_capsule_is_gone_reads_as_missing() {
    let dir = scratch("dangling");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();

    let id = view.put_object("bkt", "k", body(b"data")).await.unwrap();
    // What `spacectl delete` leaves behind for the S3 view.
    registry.delete_capsule(id).unwrap();

    assert!(matches!(
        view.head_object("bkt", "k"),
        Err(S3Error::NoSuchKey(_))
    ));
    assert!(keys(&view, "bkt").is_empty());
    view.put_object("bkt", "k", body(b"again")).await.unwrap();
    assert_eq!(read(&view, "bkt", "k").await, b"again");

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn keys_and_buckets_survive_losing_the_index() {
    let dir = scratch("lost_index");
    let (registry, view) = open(&dir);
    view.create_bucket("photos").unwrap();
    view.create_bucket("docs").unwrap();
    view.put_object("photos", "a/1.jpg", body(b"one"))
        .await
        .unwrap();
    view.put_object("photos", "a/2.jpg", body(b"two"))
        .await
        .unwrap();
    view.put_object("photos", "a/1.jpg", body(b"uno"))
        .await
        .unwrap();
    view.put_object("docs", "gone.txt", body(b"bye"))
        .await
        .unwrap();
    view.delete_object("docs", "gone.txt").await.unwrap();
    view.create_bucket("empty").unwrap();
    let capsules = registry.capsules().len();
    drop((registry, view));

    // Buckets and keys are in the capsule journal, so the index rebuilds.
    std::fs::remove_dir_all(dir.join("space.db")).unwrap();

    let (registry, view) = open(&dir);
    assert_eq!(keys(&view, "photos"), ["a/1.jpg", "a/2.jpg"]);
    assert_eq!(read(&view, "photos", "a/1.jpg").await, b"uno");
    assert_eq!(read(&view, "photos", "a/2.jpg").await, b"two");
    assert!(keys(&view, "docs").is_empty());
    view.head_bucket("empty").unwrap();
    assert_eq!(registry.capsules().len(), capsules);

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_overwrite_cut_short_leaves_the_old_object() {
    let dir = scratch("torn_overwrite");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let old = view.put_object("bkt", "k", body(b"old")).await.unwrap();
    let journal = dir.join("space.journal");
    let before = std::fs::metadata(&journal).unwrap().len();
    view.put_object("bkt", "k", body(b"new")).await.unwrap();
    drop((registry, view));

    // A crash before the overwrite's batch was whole on disk.
    let after = std::fs::metadata(&journal).unwrap().len();
    assert!(after > before + 1);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(after - 1)
        .unwrap();

    let (registry, view) = open(&dir);
    assert_eq!(read(&view, "bkt", "k").await, b"old");
    assert_eq!(view.head_object("bkt", "k").unwrap().capsule_id(), old);
    assert_eq!(registry.capsules().len(), 1);

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two create-only PUTs of one key race. Both pass the check made before
/// their bodies are stored, but the one made at the commit lets only one
/// of them win (#171).
#[tokio::test]
async fn only_one_racing_create_only_put_wins() {
    let dir = scratch("create_only");
    let (_registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    // Each body arrives only once both PUTs are waiting for one.
    let gated = |data: &'static [u8]| {
        let barrier = barrier.clone();
        stream::once(async move {
            barrier.wait().await;
            Ok::<_, std::io::Error>(Bytes::from_static(data))
        })
    };
    let create_only = Preconditions {
        if_none_match: Some("*".to_string()),
        ..Preconditions::default()
    };
    let (first, second) = tokio::join!(
        view.put_object_if("bkt", "k", gated(b"first"), &create_only),
        view.put_object_if("bkt", "k", gated(b"second"), &create_only),
    );
    let lost = [&first, &second]
        .iter()
        .filter(|put| matches!(put, Err(S3Error::PreconditionFailed)))
        .count();
    assert_eq!(lost, 1, "{first:?} {second:?}");
    let winner: &[u8] = if first.is_ok() { b"first" } else { b"second" };
    assert_eq!(read(&view, "bkt", "k").await, winner);
    assert_eq!(keys(&view, "bkt"), ["k"]);

    drop(view);
    let _ = std::fs::remove_dir_all(&dir);
}

//! Reclaim passes (#97): the data log is compacted once enough of it is
//! reclaimable, unreferenced segments under a storage root are deleted, and
//! `serve-s3` runs passes on its own. Segments a GET still reads are kept.
//! Compacting the capsule journal (#136) keeps every bucket and object.

use bytes::Bytes;
use capsule_registry::modular_pipeline::{
    registry_pipeline_from_fs_backend, AutoFsBackend, ReclaimReport,
};
use capsule_registry::CapsuleRegistry;
use common::traits::{StorageBackend, StorageTransaction};
use common::{Segment, SegmentId};
use futures::{stream, StreamExt, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::{server::spawn_reclaim, ListQuery, ObjectBody, S3Error, S3View};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const MIB: usize = 1024 * 1024;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("s3_reclaim_{name}_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = (seed << 1) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn body(data: Vec<u8>) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream::iter(
        data.chunks(MIB)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect::<Vec<_>>(),
    )
}

async fn collect(object: ObjectBody) -> Vec<u8> {
    object
        .body
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap()
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..300 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A store on the NVRAM log, as `spacectl serve-s3` opens one by default.
fn nvram_store(dir: &std::path::Path) -> (NvramLog, Arc<S3View>) {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry, log.clone()).unwrap();
    view.create_bucket("bkt").unwrap();
    (log, Arc::new(view))
}

#[tokio::test]
async fn a_pass_compacts_once_a_quarter_of_the_log_is_reclaimable() {
    let dir = scratch("threshold");
    let (log, view) = nvram_store(&dir);
    let keep = noise(2 * MIB, 1);
    view.put_object("bkt", "keep", body(keep.clone()))
        .await
        .unwrap();
    view.put_object("bkt", "small", body(noise(MIB, 2)))
        .await
        .unwrap();
    view.put_object("bkt", "big", body(noise(5 * MIB, 3)))
        .await
        .unwrap();
    assert_eq!(view.reclaim().await.unwrap(), ReclaimReport::default());

    // Less than a segment's worth to gain.
    view.delete_object("bkt", "small").await.unwrap();
    assert_eq!(view.reclaim().await.unwrap(), ReclaimReport::default());
    assert_eq!(log.file_header().generation, 0);

    view.delete_object("bkt", "big").await.unwrap();
    let before = log.size();
    let reclaimable = log.reclaimable_bytes();
    let report = view.reclaim().await.unwrap();
    assert_eq!(report.compacted, Some(reclaimable));
    assert!(reclaimable >= 6 * MIB as u64);
    assert_eq!(log.size(), before - reclaimable);
    assert_eq!(log.file_header().generation, 1);
    assert_eq!(
        collect(view.get_object("bkt", "keep").await.unwrap()).await,
        keep
    );

    drop((log, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn under_a_storage_root_a_pass_deletes_unreferenced_segments() {
    let dir = scratch("fs");
    let storage = AutoFsBackend::open(dir.join("storage")).await.unwrap();
    let mut raw = storage.clone();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let view =
        S3View::new_modular(registry_pipeline_from_fs_backend(storage, registry.clone()).unwrap());
    view.create_bucket("bkt").unwrap();

    let old = noise(5 * MIB, 4);
    let old_id = view
        .put_object("bkt", "k", body(old.clone()))
        .await
        .unwrap();
    let old_segments = registry.get_capsule(old_id).unwrap().unwrap().segments;
    view.put_object("bkt", "other", body(noise(MIB, 5)))
        .await
        .unwrap();

    // A GET holds the old object while an overwrite drops it.
    let mut object = view.get_object("bkt", "k").await.unwrap();
    let mut got = object.body.next().await.unwrap().unwrap().to_vec();
    view.put_object("bkt", "k", body(b"new".to_vec()))
        .await
        .unwrap();

    // A segment left by a write that never committed.
    let orphan = SegmentId(1_000_000);
    let mut txn = raw.begin_txn().await.unwrap();
    txn.append(orphan, b"orphan").await.unwrap();
    txn.set_segment_metadata(
        orphan,
        Segment {
            id: orphan,
            len: 6,
            ref_count: 1,
            ..Segment::default()
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let report = view.reclaim().await.unwrap();
    assert_eq!(report.collected, 1);
    assert_eq!(report.compacted, None);
    assert!(raw.metadata(orphan).await.is_err());
    for seg in &old_segments {
        assert!(raw.metadata(*seg).await.is_ok(), "held segment collected");
    }
    while let Some(chunk) = object.body.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert!(got == old, "GET in flight returned the wrong bytes");

    // Once the GET is done, releasing the old capsule deletes its segments.
    // The release runs in the background and deletes one segment at a time,
    // so wait for all of them.
    drop(object);
    let mut remaining = old_segments.clone();
    for _ in 0..300 {
        let mut still = Vec::new();
        for seg in remaining {
            if raw.metadata(seg).await.is_ok() {
                still.push(seg);
            }
        }
        remaining = still;
        if remaining.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(remaining.is_empty(), "segments {remaining:?} stayed");
    assert_eq!(view.reclaim().await.unwrap().collected, 0);
    assert_eq!(
        collect(view.get_object("bkt", "k").await.unwrap()).await,
        b"new"
    );

    drop((registry, view));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_background_loop_reclaims_on_its_own() {
    let dir = scratch("loop");
    let (log, view) = nvram_store(&dir);
    view.put_object("bkt", "keep", body(noise(MIB, 6)))
        .await
        .unwrap();
    view.put_object("bkt", "gone", body(noise(5 * MIB, 7)))
        .await
        .unwrap();

    let task = spawn_reclaim(Arc::clone(&view), Duration::from_millis(20));
    view.delete_object("bkt", "gone").await.unwrap();
    until("a pass to compact the log", || {
        log.file_header().generation == 1
    })
    .await;
    assert!(log.size() < 2 * MIB as u64);
    task.abort();

    drop((log, view));
    let _ = std::fs::remove_dir_all(&dir);
}

fn keys(view: &S3View) -> Vec<String> {
    view.list_objects("bkt", &ListQuery::default())
        .unwrap()
        .objects
        .iter()
        .map(|object| object.key().to_string())
        .collect()
}

#[tokio::test]
async fn compacting_the_journal_keeps_every_object() {
    let dir = scratch("journal");
    let latest = noise(3000, 11);
    let listed = {
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        let log = NvramLog::open(dir.join("space.nvram")).unwrap();
        let view = S3View::new(registry.clone(), log).unwrap();
        view.create_bucket("bkt").unwrap();
        for (seed, key) in ["a", "b", "c", "dir/d"].into_iter().enumerate() {
            view.put_object("bkt", key, body(noise(1000, seed as u64)))
                .await
                .unwrap();
        }
        view.put_object("bkt", "a", body(noise(2000, 10)))
            .await
            .unwrap();
        view.put_object("bkt", "a", body(latest.clone()))
            .await
            .unwrap();
        view.delete_object("bkt", "b").await.unwrap();
        let listed = keys(&view);
        assert_eq!(listed, ["a", "c", "dir/d"]);

        // Two overwritten capsules, one deleted, and their keys' records.
        let done = registry
            .compact_journal()
            .unwrap()
            .expect("nothing reclaimed");
        assert_eq!(done.generation, 1);
        assert!(done.reclaimed() > 0, "{done:?}");
        assert_eq!(keys(&view), listed);
        listed
    };

    // Rebuild the index from the compacted journal.
    std::fs::remove_dir_all(dir.join("space.db")).unwrap();
    let (log, view) = nvram_store(&dir);
    assert_eq!(keys(&view), listed);
    assert_eq!(
        collect(view.get_object("bkt", "a").await.unwrap()).await,
        latest
    );
    assert_eq!(
        collect(view.get_object("bkt", "dir/d").await.unwrap()).await,
        noise(1000, 3)
    );
    assert!(matches!(
        view.get_object("bkt", "b").await,
        Err(S3Error::NoSuchKey(_))
    ));

    drop((log, view));
    let _ = std::fs::remove_dir_all(&dir);
}

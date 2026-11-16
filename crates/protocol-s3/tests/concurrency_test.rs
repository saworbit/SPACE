//! Concurrent PUTs on a multi-threaded runtime, as `serve-s3` runs (#198).
//! Each chunk is compressed, hashed and encrypted with the pipeline
//! unlocked, so the writes and the GETs beside them share the lock only to
//! look a hash up and store a segment.

use bytes::Bytes;
use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use futures::TryStreamExt;
use nvram_sim::NvramLog;
use protocol_s3::S3View;
use std::sync::Arc;
use std::time::Duration;
use test_support::TempDir;

const MIB: usize = 1024 * 1024;

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

async fn read(view: &S3View, key: &str) -> Vec<u8> {
    let object = view.get_object("bkt", key).await.unwrap();
    object
        .body
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap()
}

/// A body that sends its first `held` bytes, then waits for `go` before
/// sending the rest.
fn held_body(
    data: Vec<u8>,
    held: usize,
    go: tokio::sync::watch::Receiver<bool>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    let data = Bytes::from(data);
    let pieces = vec![data.slice(..held), data.slice(held..)];
    futures::stream::unfold(
        (pieces.into_iter(), go, true),
        |(mut pieces, mut go, first)| async move {
            if !first {
                let _ = go.wait_for(|go| *go).await;
            }
            let piece = pieces.next()?;
            Some((Ok(piece), (pieces, go, false)))
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_puts_and_gets_share_the_store() {
    let dir = TempDir::new("s3_concurrency");
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let key = StoreSecurity::with_key(KeyManager::new([3; MASTER_KEY_SIZE]));
    let view = Arc::new(S3View::new(registry.clone(), log, &key).unwrap());
    view.create_bucket("bkt").unwrap();
    let existing = noise(3 * MIB, 99);
    view.put_object(
        "bkt",
        "existing",
        futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from(existing.clone()))]),
    )
    .await
    .unwrap();

    // Four PUTs of 13 MiB; two of them the same bytes, which must dedup.
    let bodies = [
        noise(13 * MIB, 1),
        noise(13 * MIB, 2),
        noise(13 * MIB, 1),
        noise(13 * MIB, 3),
    ];
    let (go, wait) = tokio::sync::watch::channel(false);
    let mut puts = Vec::new();
    for (i, data) in bodies.iter().enumerate() {
        let view = Arc::clone(&view);
        let body = held_body(data.clone(), 9 * MIB, wait.clone());
        puts.push(tokio::spawn(async move {
            view.put_object("bkt", &format!("k{i}"), body)
                .await
                .unwrap()
        }));
    }

    // While every PUT is part way through its body, a GET still answers.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let got = tokio::time::timeout(Duration::from_secs(20), read(&view, "existing"))
        .await
        .expect("a GET waited behind the PUTs");
    assert!(got == existing, "the existing object changed");

    go.send(true).unwrap();
    let mut ids = Vec::new();
    for put in puts {
        ids.push(put.await.unwrap());
    }
    for (i, data) in bodies.iter().enumerate() {
        assert!(read(&view, &format!("k{i}")).await == *data, "k{i} differs");
    }
    let segments = |id| registry.get_capsule(id).unwrap().unwrap().segments;
    assert_eq!(
        segments(ids[0]),
        segments(ids[2]),
        "equal PUTs didn't dedup"
    );
    assert_ne!(segments(ids[0]), segments(ids[1]));

    // After a restart and an index rebuild, everything still reads.
    drop((registry, view));
    std::fs::remove_file(dir.join("space.db")).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry, log, &key).unwrap();
    for (i, data) in bodies.iter().enumerate() {
        assert!(
            read(&view, &format!("k{i}")).await == *data,
            "k{i} differs after a rebuild"
        );
    }
    drop(view);
}

//! `serve-s3` scrubs the store it serves (#97): damage in an object's
//! segment shows up in the scrub state while the server keeps running.

use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use common::scrub::{ScrubConfig, ScrubKind, ScrubState};
use futures::stream;
use nvram_sim::NvramLog;
use protocol_s3::S3View;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

fn body(data: Vec<u8>) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream::once(futures::future::ready(Ok(Bytes::from(data))))
}

#[tokio::test]
async fn a_scheduled_scrub_finds_a_damaged_object() {
    let dir = std::env::temp_dir().join(format!("s3_scrub_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry.clone(), log.clone()).unwrap();
    view.create_bucket("bkt").unwrap();

    let damaged = view
        .put_object("bkt", "damaged", body(vec![7u8; 50_000]))
        .await
        .unwrap();
    view.put_object("bkt", "fine", body(vec![9u8; 50_000]))
        .await
        .unwrap();

    // Rot a byte of the first object's stored bytes under the running view.
    let segment = registry.lookup(damaged).unwrap().segments[0];
    let meta = log.get_segment_metadata(segment).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("space.nvram"))
        .unwrap();
    let mut byte = [0u8];
    file.seek(SeekFrom::Start(meta.offset)).unwrap();
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(meta.offset)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);

    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(3600),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let (task, mut state) = view.spawn_scrub(config).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            state.changed().await.unwrap();
            if let ScrubState::Completed { kind, errors } = *state.borrow() {
                return (kind, errors);
            }
        }
    })
    .await
    .expect("no scrub cycle completed");
    assert_eq!(outcome, (ScrubKind::Deep, 1));
    task.abort();

    drop((registry, log, view));
    let _ = std::fs::remove_dir_all(&dir);
}

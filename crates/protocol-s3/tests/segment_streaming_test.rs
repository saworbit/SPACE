//! PUT and GET move one segment at a time (#99): a failed or dropped upload
//! leaves nothing behind, a GET in flight survives an overwrite, and ranges
//! cross segment boundaries.

use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::{stream, StreamExt, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::{ByteRange, S3Error, S3View};
use std::path::PathBuf;
use std::time::Duration;

const MIB: usize = 1024 * 1024;

struct Scratch {
    dir: PathBuf,
    registry: CapsuleRegistry,
    view: S3View,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("s3_segments_{name}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        let view = S3View::new(
            registry.clone(),
            NvramLog::open(dir.join("space.nvram")).unwrap(),
        )
        .unwrap();
        Self {
            dir,
            registry,
            view,
        }
    }

    /// Wait for a background cleanup to finish.
    async fn until(&self, what: &str, done: impl Fn(&CapsuleRegistry) -> bool) {
        for _ in 0..200 {
            if done(&self.registry) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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

async fn collect(object: protocol_s3::ObjectBody) -> Vec<u8> {
    object
        .body
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_get_in_flight_keeps_reading_the_object_it_started_on() {
    let s = Scratch::new("lease");
    let old = noise(9 * MIB, 1);
    let old_id = s
        .view
        .put_object("b", "k", body(old.clone()))
        .await
        .unwrap();

    let mut object = s.view.get_object("b", "k").await.unwrap();
    let mut got = object.body.next().await.unwrap().unwrap().to_vec();

    s.view
        .put_object("b", "k", body(b"new".to_vec()))
        .await
        .unwrap();
    assert!(
        s.registry.get_capsule(old_id).unwrap().is_some(),
        "capsule deleted under a reader"
    );
    while let Some(chunk) = object.body.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert!(got == old, "GET in flight returned the wrong bytes");

    drop(object);
    s.until("the replaced capsule to go", |r| {
        r.get_capsule(old_id).unwrap().is_none()
    })
    .await;
    assert_eq!(s.registry.capsules().len(), 1);
    assert_eq!(
        collect(s.view.get_object("b", "k").await.unwrap()).await,
        b"new"
    );
}

#[tokio::test]
async fn a_failed_upload_stores_nothing() {
    let s = Scratch::new("failed");
    let chunks = vec![
        Ok(Bytes::from(noise(5 * MIB, 2))),
        Err(std::io::Error::other("client went away")),
    ];
    let result = s.view.put_object("b", "k", stream::iter(chunks)).await;

    assert!(matches!(result, Err(S3Error::Internal(_))));
    assert!(s.registry.capsules().is_empty());
    assert!(s.registry.content_entries().is_empty(), "segments leaked");
    assert!(matches!(
        s.view.head_object("b", "k"),
        Err(S3Error::NoSuchKey(_))
    ));
}

#[tokio::test]
async fn a_dropped_upload_is_cleaned_up() {
    let s = Scratch::new("dropped");
    let first = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(noise(
        5 * MIB,
        3,
    )))]);
    let stalled = first.chain(stream::pending());

    let put = s.view.put_object("b", "k", stalled);
    assert!(tokio::time::timeout(Duration::from_millis(500), put)
        .await
        .is_err());

    s.until("the dropped write to be aborted", |r| {
        r.content_entries().is_empty()
    })
    .await;
    assert!(s.registry.capsules().is_empty());
}

#[tokio::test]
async fn ranges_cross_segment_boundaries() {
    let s = Scratch::new("ranges");
    let data = noise(9 * MIB, 4);
    s.view
        .put_object("b", "k", body(data.clone()))
        .await
        .unwrap();

    let boundary = (4 * MIB) as u64;
    for (range, first, last) in [
        (
            ByteRange::From {
                first: boundary - 10,
                last: Some(boundary + 9),
            },
            boundary - 10,
            boundary + 9,
        ),
        (
            ByteRange::From {
                first: 2 * boundary,
                last: None,
            },
            2 * boundary,
            data.len() as u64 - 1,
        ),
        (
            ByteRange::Suffix(5),
            data.len() as u64 - 5,
            data.len() as u64 - 1,
        ),
    ] {
        let object = s
            .view
            .get_object_range("b", "k", Some(range))
            .await
            .unwrap();
        assert_eq!(object.range, Some((first, last)));
        let got = collect(object).await;
        assert!(
            got == data[first as usize..=last as usize],
            "wrong bytes for {range:?}"
        );
    }

    let past_end = ByteRange::From {
        first: data.len() as u64,
        last: None,
    };
    assert!(matches!(
        s.view.get_object_range("b", "k", Some(past_end)).await,
        Err(S3Error::InvalidRange(size)) if size == data.len() as u64
    ));
}

#[test]
fn range_headers_parse_like_rfc_9110() {
    let from = |first, last| Some(ByteRange::From { first, last });
    assert_eq!(ByteRange::parse("bytes=0-499"), from(0, Some(499)));
    assert_eq!(ByteRange::parse("bytes=500-"), from(500, None));
    assert_eq!(ByteRange::parse("bytes=-500"), Some(ByteRange::Suffix(500)));
    assert_eq!(ByteRange::parse(" bytes=1-2 "), from(1, Some(2)));
    for ignored in [
        "bytes=5-3",
        "bytes=0-1,4-5",
        "items=0-1",
        "bytes=a-b",
        "bytes=-",
    ] {
        assert_eq!(ByteRange::parse(ignored), None, "{ignored}");
    }

    assert_eq!(ByteRange::Suffix(500).resolve(100), Some((0, 99)));
    assert_eq!(ByteRange::Suffix(0).resolve(100), None);
    assert_eq!(
        ByteRange::From {
            first: 0,
            last: None
        }
        .resolve(0),
        None
    );
}

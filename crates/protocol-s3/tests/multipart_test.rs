//! S3 multipart upload (#201): parts uploaded in any order and of any size
//! become one object without being written again, an open upload survives
//! a restart and an index rebuild, and an aborted or abandoned one leaves
//! nothing behind.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use futures::{stream, TryStreamExt};
use nvram_sim::NvramLog;
use protocol_s3::{server, ByteRange, PutOptions, S3Error, S3View, MIN_PART_SIZE};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use test_support::TempDir;
use tower::ServiceExt;

const MIB: usize = 1024 * 1024;

fn scratch(name: &str) -> TempDir {
    TempDir::new(&format!("s3_multipart_{name}"))
}

fn open_with(dir: &Path, security: &StoreSecurity) -> (CapsuleRegistry, S3View) {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry.clone(), log, security).unwrap();
    (registry, view)
}

fn open(dir: &Path) -> (CapsuleRegistry, S3View) {
    open_with(dir, &StoreSecurity::none())
}

fn body(data: &[u8]) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream::once(futures::future::ready(Ok(Bytes::copy_from_slice(data))))
}

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

async fn read(view: &S3View, bucket: &str, key: &str, range: Option<ByteRange>) -> Vec<u8> {
    let object = view.get_object_range(bucket, key, range).await.unwrap();
    object
        .body
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap()
}

async fn part(view: &S3View, key: &str, upload: &str, number: u32, data: &[u8]) -> String {
    let id = view
        .upload_part(
            "bkt",
            key,
            upload,
            number,
            body(data),
            &PutOptions::default(),
        )
        .await
        .unwrap();
    format!("\"{}\"", id.as_uuid())
}

/// Parts uploaded out of order, none of them a whole number of 4 MiB
/// segments, make one object: its bytes, digest and headers are right, a
/// range across the parts' seams reads right, the parts are gone, and the
/// data log didn't grow at completion.
async fn complete_and_check(security: StoreSecurity, name: &str) {
    let dir = scratch(name);
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry.clone(), log.clone(), &security).unwrap();
    view.create_bucket("bkt").unwrap();
    let parts = [noise(5 * MIB + 7, 1), noise(6 * MIB + 3, 2), noise(1000, 3)];
    let whole: Vec<u8> = parts.concat();
    let upload = view
        .create_multipart_upload(
            "bkt",
            "big.bin",
            vec![("content-type".to_string(), "application/x-test".to_string())],
        )
        .await
        .unwrap();
    let tag3 = part(&view, "big.bin", &upload, 3, &parts[2]).await;
    let tag1 = part(&view, "big.bin", &upload, 1, &parts[0]).await;
    let tag2 = part(&view, "big.bin", &upload, 2, &parts[1]).await;
    let listed = view.list_parts("bkt", "big.bin", &upload).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|p| (p.number, p.size))
            .collect::<Vec<_>>(),
        [
            (1, parts[0].len() as u64),
            (2, parts[1].len() as u64),
            (3, 1000)
        ]
    );

    let log_before = std::fs::metadata(dir.join("space.nvram")).unwrap().len();
    let id = view
        .complete_multipart_upload(
            "bkt",
            "big.bin",
            &upload,
            &[(1, tag1), (2, tag2), (3, tag3)],
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(dir.join("space.nvram")).unwrap().len(),
        log_before,
        "completing wrote the parts' bytes again"
    );

    assert_eq!(read(&view, "bkt", "big.bin", None).await, whole);
    let capsule = registry.get_capsule(id).unwrap().unwrap();
    assert_eq!(capsule.size, whole.len() as u64);
    assert_eq!(capsule.digest, Some(*blake3::hash(&whole).as_bytes()));
    let head = view.head_object("bkt", "big.bin").unwrap();
    assert_eq!(head.content_type, "application/x-test");
    // Across the seam of parts 1 and 2, and into part 3.
    let seam = parts[0].len() as u64;
    let range = ByteRange::parse(&format!("bytes={}-{}", seam - 10, seam + 10)).unwrap();
    assert_eq!(
        read(&view, "bkt", "big.bin", Some(range)).await,
        whole[seam as usize - 10..=seam as usize + 10]
    );
    let tail = ByteRange::parse("bytes=-1500").unwrap();
    assert_eq!(
        read(&view, "bkt", "big.bin", Some(tail)).await,
        whole[whole.len() - 1500..]
    );

    // The object holds the parts' segments now: compacting the data log
    // keeps them, though the parts are gone.
    log.compact().unwrap();
    assert_eq!(read(&view, "bkt", "big.bin", None).await, whole);

    // The parts and the upload are gone; the object is the only capsule.
    assert!(matches!(
        view.list_parts("bkt", "big.bin", &upload),
        Err(S3Error::NoSuchUpload(_))
    ));
    assert_eq!(registry.capsules().len(), 1);

    // It reads back after a restart and a rebuild of the index.
    drop((registry, log, view));
    std::fs::remove_file(dir.join("space.db")).unwrap();
    let (_registry, view) = open_with(&dir, &security);
    assert_eq!(read(&view, "bkt", "big.bin", None).await, whole);
    drop(view);
}

#[tokio::test]
async fn parts_make_one_object_without_being_written_again() {
    complete_and_check(StoreSecurity::none(), "plain").await;
}

#[tokio::test]
async fn parts_make_one_object_on_an_encrypted_store() {
    let key = StoreSecurity::with_key(KeyManager::new([9; MASTER_KEY_SIZE]));
    complete_and_check(key, "encrypted").await;
}

/// A part uploaded again replaces the first, ListParts shows the parts
/// there are, completing replaces an object the key held, and an abort
/// leaves nothing.
#[tokio::test]
async fn parts_are_replaced_and_uploads_aborted() {
    let dir = scratch("replace");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    view.put_object("bkt", "k", body(b"the old object"))
        .await
        .unwrap();

    let upload = view
        .create_multipart_upload("bkt", "k", Vec::new())
        .await
        .unwrap();
    let first = part(&view, "k", &upload, 1, b"first try").await;
    let second = part(&view, "k", &upload, 1, b"second try").await;
    assert_ne!(first, second);
    let listed = view.list_parts("bkt", "k", &upload).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(format!("\"{}\"", listed[0].capsule_id.as_uuid()), second);
    // The old object and the second try.
    assert_eq!(registry.capsules().len(), 2);

    view.abort_multipart_upload("bkt", "k", &upload)
        .await
        .unwrap();
    assert!(matches!(
        view.list_parts("bkt", "k", &upload),
        Err(S3Error::NoSuchUpload(_))
    ));
    assert!(matches!(
        view.abort_multipart_upload("bkt", "k", &upload).await,
        Err(S3Error::NoSuchUpload(_))
    ));
    assert_eq!(registry.capsules().len(), 1);
    assert_eq!(read(&view, "bkt", "k", None).await, b"the old object");

    // Completing replaces the object, and drops a part it leaves out.
    let upload = view
        .create_multipart_upload("bkt", "k", Vec::new())
        .await
        .unwrap();
    let one = part(&view, "k", &upload, 1, b"only part").await;
    part(&view, "k", &upload, 2, b"left out").await;
    view.complete_multipart_upload("bkt", "k", &upload, &[(1, one)])
        .await
        .unwrap();
    assert_eq!(read(&view, "bkt", "k", None).await, b"only part");
    assert_eq!(registry.capsules().len(), 1);
    assert!(registry
        .list_namespaces(protocol_s3::UPLOAD_PREFIX, None, usize::MAX)
        .unwrap()
        .is_empty());

    drop((registry, view));
}

/// CompleteMultipartUpload refuses parts out of order, parts that aren't
/// there or have another ETag, and a part under 5 MiB that isn't the last.
/// An upload id is only good for its own key.
#[tokio::test]
async fn completion_checks_its_parts() {
    let dir = scratch("checks");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let upload = view
        .create_multipart_upload("bkt", "k", Vec::new())
        .await
        .unwrap();
    let small = part(&view, "k", &upload, 1, b"small").await;
    let big = part(&view, "k", &upload, 2, &noise(MIN_PART_SIZE as usize, 4)).await;

    let complete = |parts: Vec<(u32, String)>| {
        let view = &view;
        let upload = upload.clone();
        async move {
            view.complete_multipart_upload("bkt", "k", &upload, &parts)
                .await
        }
    };
    assert!(matches!(
        complete(vec![(2, big.clone()), (1, small.clone())]).await,
        Err(S3Error::InvalidPartOrder)
    ));
    assert!(matches!(
        complete(vec![(1, big.clone())]).await,
        Err(S3Error::InvalidPart(_))
    ));
    assert!(matches!(
        complete(vec![(3, big.clone())]).await,
        Err(S3Error::InvalidPart(_))
    ));
    assert!(matches!(
        complete(vec![(1, small.clone()), (2, big.clone())]).await,
        Err(S3Error::EntityTooSmall(1))
    ));
    assert!(matches!(
        complete(Vec::new()).await,
        Err(S3Error::MalformedXML(_))
    ));
    assert!(matches!(
        view.list_parts("bkt", "other", &upload),
        Err(S3Error::NoSuchUpload(_))
    ));
    assert!(matches!(
        view.list_parts("bkt", "k", "not-an-upload"),
        Err(S3Error::NoSuchUpload(_))
    ));
    assert!(matches!(
        view.upload_part("bkt", "k", &upload, 0, body(b"x"), &PutOptions::default())
            .await,
        Err(S3Error::InvalidArgument(_))
    ));
    assert!(matches!(
        view.create_multipart_upload("nobkt", "k", Vec::new()).await,
        Err(S3Error::NoSuchBucket(_))
    ));
    // Nothing failed changed the upload: a 5 MiB part alone completes.
    complete(vec![(2, big)]).await.unwrap();
    assert_eq!(
        read(&view, "bkt", "k", None).await,
        noise(MIN_PART_SIZE as usize, 4)
    );

    drop((registry, view));
}

/// An open upload is in the journal: it survives a restart and a rebuilt
/// index. A reclaim pass aborts uploads older than its age.
#[tokio::test]
async fn open_uploads_last_until_completed_or_abandoned() {
    let dir = scratch("restart");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let kept = view
        .create_multipart_upload("bkt", "kept", Vec::new())
        .await
        .unwrap();
    let tag = part(&view, "kept", &kept, 1, b"kept part").await;
    drop((registry, view));
    std::fs::remove_file(dir.join("space.db")).unwrap();

    let (registry, view) = open(&dir);
    assert_eq!(view.list_parts("bkt", "kept", &kept).unwrap().len(), 1);
    view.complete_multipart_upload("bkt", "kept", &kept, &[(1, tag)])
        .await
        .unwrap();
    assert_eq!(read(&view, "bkt", "kept", None).await, b"kept part");

    let abandoned = view
        .create_multipart_upload("bkt", "gone", Vec::new())
        .await
        .unwrap();
    part(&view, "gone", &abandoned, 1, b"abandoned part").await;
    assert_eq!(
        view.abort_uploads_older_than(Duration::from_secs(3600))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        view.abort_uploads_older_than(Duration::ZERO).await.unwrap(),
        1
    );
    assert!(matches!(
        view.list_parts("bkt", "gone", &abandoned),
        Err(S3Error::NoSuchUpload(_))
    ));
    assert_eq!(registry.capsules().len(), 1);

    drop((registry, view));
}

/// DeleteBucket aborts the bucket's open uploads in its own batch (#233):
/// their parts are dropped, and an upload id from before can't complete
/// into a new bucket of the same name. Other buckets' uploads stay, and a
/// bucket that isn't empty keeps its uploads.
#[tokio::test]
async fn deleting_a_bucket_aborts_its_open_uploads() {
    let dir = scratch("delete_bucket");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    view.create_bucket("other").unwrap();
    let upload = view
        .create_multipart_upload("bkt", "k", Vec::new())
        .await
        .unwrap();
    let tag = part(&view, "k", &upload, 1, b"part").await;
    let kept = view
        .create_multipart_upload("other", "k", Vec::new())
        .await
        .unwrap();
    let kept_tag = view
        .upload_part(
            "other",
            "k",
            &kept,
            1,
            body(b"kept"),
            &PutOptions::default(),
        )
        .await
        .unwrap();

    // Not empty: nothing changes, uploads included.
    view.put_object("bkt", "obj", body(b"object"))
        .await
        .unwrap();
    assert!(matches!(
        view.delete_bucket("bkt").await,
        Err(S3Error::BucketNotEmpty(_))
    ));
    assert_eq!(view.list_parts("bkt", "k", &upload).unwrap().len(), 1);
    view.delete_object("bkt", "obj").await.unwrap();

    view.delete_bucket("bkt").await.unwrap();
    assert_eq!(registry.capsules().len(), 1, "only the other bucket's part");
    let open_uploads = registry
        .list_namespaces(protocol_s3::UPLOAD_PREFIX, None, usize::MAX)
        .unwrap();
    assert_eq!(open_uploads.len(), 1);
    assert!(open_uploads[0].0.ends_with(&kept));

    view.create_bucket("bkt").unwrap();
    assert!(matches!(
        view.complete_multipart_upload("bkt", "k", &upload, &[(1, tag)])
            .await,
        Err(S3Error::NoSuchUpload(_))
    ));
    assert!(matches!(
        view.upload_part("bkt", "k", &upload, 2, body(b"x"), &PutOptions::default())
            .await,
        Err(S3Error::NoSuchUpload(_))
    ));
    assert!(view.head_object("bkt", "k").is_err());
    assert_eq!(registry.capsules().len(), 1);

    view.complete_multipart_upload(
        "other",
        "k",
        &kept,
        &[(1, format!("\"{}\"", kept_tag.as_uuid()))],
    )
    .await
    .unwrap();
    assert_eq!(read(&view, "other", "k", None).await, b"kept");

    drop((registry, view));
}

/// ListMultipartUploads (#234) lists a bucket's open uploads by key and
/// then upload id, filtered by prefix, and pages with `max` and the
/// markers. Other buckets' uploads, and completed or aborted ones, aren't
/// listed.
#[tokio::test]
async fn open_uploads_are_listed_by_bucket_and_paged() {
    let dir = scratch("list_uploads");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    view.create_bucket("other").unwrap();
    let start = |bucket: &'static str, key: &'static str| {
        let view = &view;
        async move {
            view.create_multipart_upload(bucket, key, Vec::new())
                .await
                .unwrap()
        }
    };
    let mut expected = vec![
        ("a/one".to_string(), start("bkt", "a/one").await),
        ("a/one".to_string(), start("bkt", "a/one").await),
        ("a/two".to_string(), start("bkt", "a/two").await),
        ("b".to_string(), start("bkt", "b").await),
    ];
    expected.sort();
    start("other", "a/one").await;
    let done = start("bkt", "a/done").await;
    let tag = part(&view, "a/done", &done, 1, b"done").await;
    view.complete_multipart_upload("bkt", "a/done", &done, &[(1, tag)])
        .await
        .unwrap();
    let aborted = start("bkt", "a/aborted").await;
    view.abort_multipart_upload("bkt", "a/aborted", &aborted)
        .await
        .unwrap();

    let listed = |page: &protocol_s3::UploadPage| -> Vec<(String, String)> {
        page.uploads
            .iter()
            .map(|u| (u.key.clone(), u.upload_id.clone()))
            .collect()
    };
    let all = view
        .list_multipart_uploads("bkt", "", None, None, None, 1000)
        .unwrap();
    assert_eq!(listed(&all), expected);
    assert!(!all.truncated);
    assert!(all.uploads.iter().all(|u| u.initiated > 0));

    let prefixed = view
        .list_multipart_uploads("bkt", "a/", None, None, None, 1000)
        .unwrap();
    assert_eq!(listed(&prefixed), expected[..3]);

    // A page at a time, carrying both markers on.
    let mut paged = Vec::new();
    let (mut key, mut id): (Option<String>, Option<String>) = (None, None);
    loop {
        let page = view
            .list_multipart_uploads("bkt", "", None, key.as_deref(), id.as_deref(), 1)
            .unwrap();
        paged.extend(listed(&page));
        if !page.truncated {
            break;
        }
        let last = page.uploads.last().unwrap();
        (key, id) = (Some(last.key.clone()), Some(last.upload_id.clone()));
    }
    assert_eq!(paged, expected);

    // A key marker alone skips every upload of that key.
    let after_key = view
        .list_multipart_uploads("bkt", "", None, Some("a/one"), None, 1000)
        .unwrap();
    assert_eq!(listed(&after_key), expected[2..]);
    // The second upload of a key, after the first.
    let after_first = view
        .list_multipart_uploads("bkt", "", None, Some("a/one"), Some(&expected[0].1), 1)
        .unwrap();
    assert_eq!(listed(&after_first), expected[1..2]);
    assert!(after_first.truncated);

    assert!(matches!(
        view.list_multipart_uploads("nobkt", "", None, None, None, 1000),
        Err(S3Error::NoSuchBucket(_))
    ));

    drop((registry, view));
}

/// Uploads of one key are listed in the order they were started, as in S3,
/// and paging by upload id keeps that order (#253).
#[tokio::test]
async fn uploads_of_a_key_list_in_the_order_they_were_started() {
    let dir = scratch("uploads_in_start_order");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let mut started = Vec::new();
    for _ in 0..8 {
        started.push(
            view.create_multipart_upload("bkt", "same", Vec::new())
                .await
                .unwrap(),
        );
    }

    let ids = |page: &protocol_s3::UploadPage| -> Vec<String> {
        page.uploads.iter().map(|u| u.upload_id.clone()).collect()
    };
    let all = view
        .list_multipart_uploads("bkt", "", None, None, None, 1000)
        .unwrap();
    assert_eq!(ids(&all), started);

    let mut paged = Vec::new();
    let (mut key, mut id) = (None, None);
    loop {
        let page = view
            .list_multipart_uploads("bkt", "", None, key.as_deref(), id.as_deref(), 3)
            .unwrap();
        paged.extend(ids(&page));
        if !page.truncated {
            break;
        }
        (key, id) = (page.next_key_marker, page.next_upload_id_marker);
    }
    assert_eq!(paged, started);

    drop((registry, view));
}

/// With a delimiter, keys that hold it past the prefix are rolled into
/// common prefixes, which count toward the page size and page like keys
/// (#253).
#[tokio::test]
async fn a_delimiter_rolls_upload_keys_into_common_prefixes() {
    let dir = scratch("uploads_delimiter");
    let (registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let mut ids = std::collections::HashMap::new();
    for key in ["a/one", "a/two", "a/two", "b", "c/x/y", "c/z"] {
        let id = view
            .create_multipart_upload("bkt", key, Vec::new())
            .await
            .unwrap();
        ids.entry(key).or_insert(id);
    }
    let keys = |page: &protocol_s3::UploadPage| -> Vec<String> {
        page.uploads.iter().map(|u| u.key.clone()).collect()
    };

    let all = view
        .list_multipart_uploads("bkt", "", Some("/"), None, None, 1000)
        .unwrap();
    assert_eq!(keys(&all), ["b"]);
    assert_eq!(all.common_prefixes, ["a/", "c/"]);
    assert!(!all.truncated);

    let under_c = view
        .list_multipart_uploads("bkt", "c/", Some("/"), None, None, 1000)
        .unwrap();
    assert_eq!(keys(&under_c), ["c/z"]);
    assert_eq!(under_c.common_prefixes, ["c/x/"]);

    // An empty delimiter groups nothing, as with no delimiter.
    let flat = view
        .list_multipart_uploads("bkt", "", Some(""), None, None, 1000)
        .unwrap();
    assert_eq!(flat.uploads.len(), 6);
    assert!(flat.common_prefixes.is_empty());

    // One entry a page: a common prefix, then an upload, then a common
    // prefix. A page that ends on a common prefix gives no upload id marker.
    let first = view
        .list_multipart_uploads("bkt", "", Some("/"), None, None, 1)
        .unwrap();
    assert_eq!(first.common_prefixes, ["a/"]);
    assert!(first.uploads.is_empty() && first.truncated);
    assert_eq!(first.next_key_marker.as_deref(), Some("a/"));
    assert_eq!(first.next_upload_id_marker, None);
    let second = view
        .list_multipart_uploads("bkt", "", Some("/"), Some("a/"), None, 1)
        .unwrap();
    assert_eq!(keys(&second), ["b"]);
    assert!(second.common_prefixes.is_empty() && second.truncated);
    assert_eq!(second.next_key_marker.as_deref(), Some("b"));
    assert_eq!(second.next_upload_id_marker.as_ref(), Some(&ids["b"]));
    let third = view
        .list_multipart_uploads(
            "bkt",
            "",
            Some("/"),
            second.next_key_marker.as_deref(),
            second.next_upload_id_marker.as_deref(),
            1,
        )
        .unwrap();
    assert!(third.uploads.is_empty());
    assert_eq!(third.common_prefixes, ["c/"]);
    assert!(!third.truncated);

    // Two a page: the last common prefix fills the page exactly, so it isn't
    // truncated.
    let page = view
        .list_multipart_uploads("bkt", "", Some("/"), Some("a/"), None, 2)
        .unwrap();
    assert_eq!(keys(&page), ["b"]);
    assert_eq!(page.common_prefixes, ["c/"]);
    assert!(!page.truncated);

    drop((registry, view));
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> &'a str {
    let start = text
        .find(open)
        .unwrap_or_else(|| panic!("{open} in {text}"))
        + open.len();
    &text[start..start + text[start..].find(close).unwrap()]
}

/// The routes an SDK uses: POST ?uploads, PUT ?partNumber&uploadId, GET
/// ?uploadId, POST ?uploadId with the XML parts list, and DELETE ?uploadId.
#[tokio::test]
async fn the_multipart_routes_work_over_http() {
    let dir = scratch("http");
    let (_registry, view) = open(&dir);
    view.create_bucket("bkt").unwrap();
    let app = server::router(Arc::new(view));
    let host = |r: axum::http::request::Builder| r.header("host", "127.0.0.1");

    let (status, text) = send(
        &app,
        host(Request::post("/bkt/dir/obj.bin?uploads"))
            .header("content-type", "text/plain")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let upload = between(&text, "<UploadId>", "</UploadId>").to_string();

    let parts = [noise(5 * MIB + 1, 5), noise(10, 6)];
    let mut tags = Vec::new();
    for (i, data) in parts.iter().enumerate() {
        let response = app
            .clone()
            .oneshot(
                host(Request::put(format!(
                    "/bkt/dir/obj.bin?partNumber={}&uploadId={upload}",
                    i + 1
                )))
                .body(Body::from(data.clone()))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tags.push(response.headers()["etag"].to_str().unwrap().to_string());
    }

    let (status, text) = send(
        &app,
        host(Request::get(format!("/bkt/dir/obj.bin?uploadId={upload}")))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(text.matches("<Part>").count(), 2, "{text}");
    assert!(text.contains(tags[1].trim_matches('"')), "{text}");

    // ListMultipartUploads shows the open upload (#234).
    let list_uploads = || {
        host(Request::get("/bkt?uploads&prefix=dir%2F&max-uploads=5"))
            .body(Body::empty())
            .unwrap()
    };
    let (status, text) = send(&app, list_uploads()).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("<ListMultipartUploadsResult"), "{text}");
    let listed = between(&text, "<Upload>", "</Upload>");
    assert!(listed.contains("<Key>dir/obj.bin</Key>"), "{text}");
    assert!(
        listed.contains(&format!("<UploadId>{upload}</UploadId>")),
        "{text}"
    );
    assert!(text.contains("<MaxUploads>5</MaxUploads>"), "{text}");
    assert!(text.contains("<IsTruncated>false</IsTruncated>"), "{text}");
    // With a delimiter, the key rolls into its common prefix (#253).
    let (status, text) = send(
        &app,
        host(Request::get("/bkt?uploads&delimiter=%2F"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("<Delimiter>/</Delimiter>"), "{text}");
    assert!(!text.contains("<Upload>"), "{text}");
    assert!(
        text.contains("<CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes>"),
        "{text}"
    );
    assert!(
        text.contains("<NextKeyMarker>dir/</NextKeyMarker>"),
        "{text}"
    );
    assert!(!text.contains("<NextUploadIdMarker>"), "{text}");

    let xml = format!(
        "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Part><PartNumber>1</PartNumber><ETag>{}</ETag><ChecksumCRC32>AAAAAA==</ChecksumCRC32></Part>\
         <Part><PartNumber>2</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
        tags[0], tags[1]
    );
    let (status, text) = send(
        &app,
        host(Request::post(format!("/bkt/dir/obj.bin?uploadId={upload}")))
            .body(Body::from(xml))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("<CompleteMultipartUploadResult"), "{text}");
    assert!(text.contains("<Key>dir/obj.bin</Key>"), "{text}");
    let (status, text) = send(&app, list_uploads()).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains("<Upload>"), "{text}");

    let response = app
        .clone()
        .oneshot(
            host(Request::get("/bkt/dir/obj.bin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/plain");
    let got = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(got[..] == parts.concat()[..], "the object's bytes differ");

    // An aborted upload is gone, and a bad document is MalformedXML.
    let (_, text) = send(
        &app,
        host(Request::post("/bkt/other?uploads"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let other = between(&text, "<UploadId>", "</UploadId>").to_string();
    let (status, text) = send(
        &app,
        host(Request::post(format!("/bkt/other?uploadId={other}")))
            .body(Body::from("<Complete/>"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("MalformedXML"), "{text}");
    let (status, _) = send(
        &app,
        host(Request::delete(format!("/bkt/other?uploadId={other}")))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, text) = send(
        &app,
        host(Request::get(format!("/bkt/other?uploadId={other}")))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert!(text.contains("NoSuchUpload"), "{text}");

    drop(app);
}

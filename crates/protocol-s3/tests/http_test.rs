//! The S3 routes, driven through the real router.

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use capsule_registry::CapsuleRegistry;
use nvram_sim::NvramLog;
use protocol_s3::{server, S3View};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

struct App {
    router: Router,
    dir: PathBuf,
}

impl App {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("s3_http_{name}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let view = S3View::new(
            CapsuleRegistry::open(dir.join("space.db")).unwrap(),
            NvramLog::open(dir.join("space.nvram")).unwrap(),
        )
        .unwrap();
        Self {
            router: server::router(Arc::new(view)),
            dir,
        }
    }

    async fn send(&self, method: &str, uri: &str, body: &[u8]) -> (StatusCode, HeaderMap, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::from(body.to_vec()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Text of every `<tag>` element, in order.
fn tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.split(open.as_str())
        .skip(1)
        .map(|rest| rest.split(close.as_str()).next().unwrap())
        .collect()
}

#[tokio::test]
async fn keys_with_slashes_work_on_every_method() {
    let app = App::new("nested");
    let uri = "/demo/dir/sub/file.txt";

    let (status, _, _) = app.send("PUT", uri, b"nested").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = app.send("GET", uri, b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "nested"));
    let (status, headers, _) = app.send("HEAD", uri, b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_LENGTH], "6");

    let (_, _, xml) = app.send("GET", "/demo?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["dir/sub/file.txt"]);

    let (status, _, _) = app.send("DELETE", uri, b"").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = app.send("GET", uri, b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_sends_length_etag_and_last_modified() {
    let app = App::new("headers");
    let (_, put_headers, _) = app.send("PUT", "/demo/a.txt", b"hello").await;

    let (status, headers, body) = app.send("GET", "/demo/a.txt", b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "hello"));
    assert_eq!(headers[header::CONTENT_LENGTH], "5");
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain");
    assert_eq!(headers[header::ETAG], put_headers[header::ETAG]);
    assert!(headers.contains_key(header::LAST_MODIFIED));
}

#[tokio::test]
async fn list_objects_v2_returns_s3_xml() {
    let app = App::new("list");
    for key in ["b.txt", "a.txt", "dir/x.txt", "dir/y.txt", "z/1.txt"] {
        app.send("PUT", &format!("/demo/{key}"), key.as_bytes())
            .await;
    }

    let (status, headers, xml) = app.send("GET", "/demo?list-type=2", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/xml");
    assert!(xml.contains("<ListBucketResult"), "{xml}");
    assert_eq!(tags(&xml, "Name"), ["demo"]);
    assert_eq!(
        tags(&xml, "Key"),
        ["a.txt", "b.txt", "dir/x.txt", "dir/y.txt", "z/1.txt"]
    );
    assert_eq!(tags(&xml, "KeyCount"), ["5"]);
    assert_eq!(tags(&xml, "IsTruncated"), ["false"]);
    assert_eq!(tags(&xml, "Size")[0], "5");
    assert!(tags(&xml, "LastModified")[0].ends_with(".000Z"));

    let (_, _, xml) = app.send("GET", "/demo?list-type=2&delimiter=/", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a.txt", "b.txt"]);
    assert_eq!(tags(&xml, "CommonPrefixes").len(), 2);
    assert!(xml.contains("<CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes>"));

    let (_, _, xml) = app.send("GET", "/demo?list-type=2&prefix=dir/", b"").await;
    assert_eq!(tags(&xml, "Key"), ["dir/x.txt", "dir/y.txt"]);

    let (_, _, xml) = app
        .send("GET", "/demo?list-type=2&prefix=dir/&start-after=a", b"")
        .await;
    assert_eq!(tags(&xml, "Key"), ["dir/x.txt", "dir/y.txt"]);
}

#[tokio::test]
async fn list_pages_follow_the_continuation_token() {
    let app = App::new("pages");
    for key in ["a", "b", "dir/1", "dir/2", "dir/3", "e"] {
        app.send("PUT", &format!("/demo/{key}"), b"x").await;
    }

    let mut seen = Vec::new();
    let mut uri = "/demo?list-type=2&delimiter=/&max-keys=2".to_string();
    for _ in 0..10 {
        let (status, _, xml) = app.send("GET", &uri, b"").await;
        assert_eq!(status, StatusCode::OK, "{xml}");
        seen.extend(tags(&xml, "Key").into_iter().map(String::from));
        seen.extend(
            tags(&xml, "CommonPrefixes")
                .into_iter()
                .map(|p| p.replace("<Prefix>", "").replace("</Prefix>", "")),
        );
        match tags(&xml, "NextContinuationToken").first() {
            Some(token) => {
                assert_eq!(tags(&xml, "IsTruncated"), ["true"]);
                uri =
                    format!("/demo?list-type=2&delimiter=/&max-keys=2&continuation-token={token}");
            }
            None => break,
        }
    }
    seen.sort();
    assert_eq!(seen, ["a", "b", "dir/", "e"]);

    let (status, _, xml) = app
        .send("GET", "/demo?list-type=2&continuation-token=zz", b"")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(tags(&xml, "Code"), ["InvalidArgument"]);
}

#[tokio::test]
async fn list_objects_v1_uses_markers() {
    let app = App::new("v1");
    for key in ["a", "b", "c"] {
        app.send("PUT", &format!("/demo/{key}"), b"x").await;
    }
    let (_, _, xml) = app.send("GET", "/demo?max-keys=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a", "b"]);
    assert_eq!(tags(&xml, "NextMarker"), ["b"]);
    let (_, _, xml) = app.send("GET", "/demo?marker=b", b"").await;
    assert_eq!(tags(&xml, "Key"), ["c"]);
    assert_eq!(tags(&xml, "IsTruncated"), ["false"]);
}

#[tokio::test]
async fn url_encoding_type_encodes_keys() {
    let app = App::new("encoding");
    app.send("PUT", "/demo/a%20b%26c.txt", b"x").await;
    let (_, _, xml) = app
        .send("GET", "/demo?list-type=2&encoding-type=url", b"")
        .await;
    assert_eq!(tags(&xml, "EncodingType"), ["url"]);
    assert_eq!(tags(&xml, "Key"), ["a%20b%26c.txt"]);
    let (_, _, xml) = app.send("GET", "/demo?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a b&amp;c.txt"]);
}

#[tokio::test]
async fn errors_are_s3_xml() {
    let app = App::new("errors");
    app.send("PUT", "/demo/a.txt", b"x").await;

    let (status, headers, xml) = app.send("GET", "/demo/missing.txt", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[header::CONTENT_TYPE], "application/xml");
    assert_eq!(tags(&xml, "Code"), ["NoSuchKey"]);
    assert_eq!(tags(&xml, "Resource"), ["missing.txt"]);

    let (status, _, xml) = app.send("GET", "/never?list-type=2", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(tags(&xml, "Code"), ["NoSuchBucket"]);
    let (_, _, xml) = app.send("GET", "/never/a.txt", b"").await;
    assert_eq!(tags(&xml, "Code"), ["NoSuchBucket"]);

    let (status, _, xml) = app.send("GET", "/demo?max-keys=lots", b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(tags(&xml, "Code"), ["InvalidArgument"]);
}

#[tokio::test]
async fn buckets_can_be_created_and_listed_empty() {
    let app = App::new("buckets");
    let (status, _, _) = app.send("HEAD", "/fresh", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = app.send("PUT", "/fresh", b"").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = app.send("HEAD", "/fresh", b"").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, xml) = app.send("GET", "/fresh?list-type=2", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tags(&xml, "KeyCount"), ["0"]);

    // Emptying a bucket keeps it.
    app.send("PUT", "/fresh/a", b"x").await;
    app.send("DELETE", "/fresh/a", b"").await;
    let (status, _, _) = app.send("GET", "/fresh?list-type=2", b"").await;
    assert_eq!(status, StatusCode::OK);
}

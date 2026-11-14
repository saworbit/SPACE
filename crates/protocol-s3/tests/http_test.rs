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
        Self::with_store(name, |_| {})
    }

    /// An app over a store `setup` has prepared, as an older build might
    /// have left it.
    fn with_store(name: &str, setup: impl FnOnce(&CapsuleRegistry)) -> Self {
        let dir = std::env::temp_dir().join(format!("s3_http_{name}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        let log = NvramLog::open(dir.join("space.nvram")).unwrap();
        let view = S3View::new(registry.clone(), log).unwrap();
        setup(&registry);
        // Most tests use the bucket `demo`.
        view.create_bucket("demo").unwrap();
        Self {
            router: server::router(Arc::new(view)),
            dir,
        }
    }

    async fn send(&self, method: &str, uri: &str, body: &[u8]) -> (StatusCode, HeaderMap, String) {
        self.send_with(method, uri, &[], body).await
    }

    async fn send_with(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (StatusCode, HeaderMap, String) {
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Body::from(body.to_vec())).unwrap();
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

#[tokio::test]
async fn range_requests_get_partial_content() {
    let app = App::new("range");
    app.send("PUT", "/demo/digits.txt", b"0123456789").await;

    let get = |range: &'static str| {
        let router = app.router.clone();
        async move {
            let request = Request::builder()
                .uri("/demo/digits.txt")
                .header(header::RANGE, range)
                .body(Body::empty())
                .unwrap();
            let response = router.oneshot(request).await.unwrap();
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
    };

    for (range, body, content_range) in [
        ("bytes=2-4", "234", "bytes 2-4/10"),
        ("bytes=7-", "789", "bytes 7-9/10"),
        ("bytes=-3", "789", "bytes 7-9/10"),
        ("bytes=5-100", "56789", "bytes 5-9/10"),
    ] {
        let (status, headers, got) = get(range).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(got, body, "{range}");
        assert_eq!(headers[header::CONTENT_RANGE], content_range, "{range}");
        assert_eq!(headers[header::CONTENT_LENGTH], body.len().to_string());
    }

    let (status, headers, xml) = get("bytes=10-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes */10");
    assert_eq!(tags(&xml, "Code"), ["InvalidRange"]);

    // A range the server doesn't serve is ignored, as RFC 9110 allows.
    for ignored in ["bytes=0-1,4-5", "bytes=x-y"] {
        let (status, headers, got) = get(ignored).await;
        assert_eq!((status, got.as_str()), (StatusCode::OK, "0123456789"));
        assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    }
}

/// S3 picks the operation from the query and headers as well as the
/// method. Operations the server doesn't do must be refused, not run as the
/// plain PUT, GET or DELETE of the same path (#156).
#[tokio::test]
async fn unimplemented_operations_are_refused_not_misrun() {
    let app = App::new("unimplemented");
    app.send("PUT", "/demo/a.txt", b"original").await;
    app.send("PUT", "/demo/src.txt", b"source").await;

    let refused = |method: &'static str, uri: &'static str, headers, body: &'static str| {
        let app = &app;
        async move {
            let (status, _, xml) = app.send_with(method, uri, headers, body.as_bytes()).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {uri}: {xml}");
            assert_eq!(tags(&xml, "Code"), ["NotImplemented"], "{method} {uri}");
        }
    };
    for (method, uri, body) in [
        // Object subresources: tagging, ACLs, parts, response overrides.
        ("PUT", "/demo/a.txt?tagging", "<Tagging><TagSet/></Tagging>"),
        ("PUT", "/demo/a.txt?acl", "<AccessControlPolicy/>"),
        ("DELETE", "/demo/a.txt?tagging", ""),
        ("GET", "/demo/a.txt?tagging", ""),
        ("GET", "/demo/a.txt?acl", ""),
        ("GET", "/demo/a.txt?partNumber=1", ""),
        // Response overrides are for GET alone.
        ("PUT", "/demo/a.txt?response-content-type=text/html", "x"),
        ("PUT", "/demo/a.txt?x-id=PutObjectTagging", "<Tagging/>"),
        // Bucket configuration, which mustn't create the bucket either.
        ("PUT", "/demo?versioning", "<VersioningConfiguration/>"),
        ("PUT", "/other?versioning", "<VersioningConfiguration/>"),
        ("GET", "/demo?versioning", ""),
        // Operations with no route.
        ("POST", "/demo?delete", "<Delete/>"),
        ("POST", "/demo/big.bin?uploads", ""),
        ("DELETE", "/demo", ""),
        ("GET", "/", ""),
    ] {
        refused(method, uri, &[], body).await;
    }
    // CopyObject, and a body in aws-chunked framing.
    refused(
        "PUT",
        "/demo/dst.txt",
        &[("x-amz-copy-source", "/demo/src.txt")],
        "",
    )
    .await;
    refused(
        "PUT",
        "/demo/a.txt",
        &[
            ("x-amz-content-sha256", "STREAMING-UNSIGNED-PAYLOAD-TRAILER"),
            ("content-encoding", "aws-chunked"),
        ],
        "5\r\nhello\r\n0\r\n\r\n",
    )
    .await;

    // None of it touched the store.
    let (status, _, body) = app.send("GET", "/demo/a.txt", b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "original"));
    let (status, _, _) = app.send("HEAD", "/demo/dst.txt", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = app.send("HEAD", "/other", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, xml) = app.send("GET", "/demo?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a.txt", "src.txt"]);
}

/// Presigned URLs and the SDKs' `x-id` tag don't change the operation, so
/// they're accepted.
#[tokio::test]
async fn presigned_and_sdk_query_parameters_are_accepted() {
    let app = App::new("presigned");
    let sigv4 = "X-Amz-Algorithm=AWS4-HMAC-SHA256\
                 &X-Amz-Credential=x%2F20261113%2Fus-east-1%2Fs3%2Faws4_request\
                 &X-Amz-Date=20261113T000000Z&X-Amz-Expires=3600\
                 &X-Amz-SignedHeaders=host&X-Amz-Signature=abc";

    let uri = format!("/demo/a.txt?x-id=PutObject&{sigv4}");
    assert_eq!(app.send("PUT", &uri, b"hi").await.0, StatusCode::OK);
    let uri = format!("/demo/a.txt?x-id=GetObject&{sigv4}");
    let (status, _, body) = app.send("GET", &uri, b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "hi"));
    let uri = "/demo/a.txt?AWSAccessKeyId=x&Signature=y&Expires=1893456000";
    let (status, _, body) = app.send("GET", uri, b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "hi"));
    let (status, _, xml) = app
        .send("GET", "/demo?list-type=2&fetch-owner=true", b"")
        .await;
    assert_eq!(status, StatusCode::OK, "{xml}");
    assert_eq!(tags(&xml, "Key"), ["a.txt"]);

    // GetBucketLocation answers with the default region.
    let (status, _, xml) = app.send("GET", "/demo?location", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert!(xml.contains("<LocationConstraint"), "{xml}");
    assert!(!xml.contains("<ListBucketResult"), "{xml}");
    let (status, _, xml) = app.send("GET", "/never?location", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(tags(&xml, "Code"), ["NoSuchBucket"]);

    let uri = "/demo/a.txt?x-id=DeleteObject";
    assert_eq!(app.send("DELETE", uri, b"").await.0, StatusCode::NO_CONTENT);
}

/// DeleteObject is idempotent: a key that isn't there is already deleted,
/// so a retried DELETE succeeds (#160). A missing bucket is still an error.
#[tokio::test]
async fn deleting_a_missing_key_succeeds() {
    let app = App::new("delete_missing");
    app.send("PUT", "/demo/a.txt", b"x").await;
    for _ in 0..2 {
        let (status, _, body) = app.send("DELETE", "/demo/a.txt", b"").await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
    let (status, _, _) = app.send("DELETE", "/demo/never.txt", b"").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, xml) = app.send("DELETE", "/nobucket/a.txt", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(tags(&xml, "Code"), ["NoSuchBucket"]);
}

/// A key can't be empty, so `/{bucket}/` addresses the bucket (#179).
#[tokio::test]
async fn a_trailing_slash_still_addresses_the_bucket() {
    let app = App::new("trailing_slash");
    assert_eq!(
        app.send("HEAD", "/trail/", b"").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, headers, _) = app.send("PUT", "/trail/", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::LOCATION], "/trail");
    assert_eq!(app.send("HEAD", "/trail/", b"").await.0, StatusCode::OK);

    app.send("PUT", "/trail/dir/a.txt", b"x").await;
    let (status, _, xml) = app
        .send("GET", "/trail/?list-type=2&delimiter=/", b"")
        .await;
    assert_eq!(status, StatusCode::OK, "{xml}");
    assert_eq!(tags(&xml, "Prefix"), ["", "dir/"]);
    let (_, _, xml) = app.send("GET", "/trail/?location", b"").await;
    assert!(xml.contains("<LocationConstraint"), "{xml}");
    let (status, _, xml) = app.send("GET", "/trail/?versioning", b"").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{xml}");

    // Other methods on the bucket are still refused.
    let (status, _, xml) = app.send("DELETE", "/trail/", b"").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(tags(&xml, "Code"), ["NotImplemented"]);
}

/// XML 1.0 can't carry most control characters even escaped, so keys
/// holding one are refused rather than breaking every listing of their
/// bucket (#172). Tab, newline and carriage return list intact.
#[tokio::test]
async fn keys_a_listing_cant_carry_are_refused() {
    let app = App::new("control_chars");
    app.send("PUT", "/ctl", b"").await;
    for bad in ["bad%01key", "bad%00key", "bad%1Fkey", "bad%EF%BF%BEkey"] {
        let (status, _, xml) = app.send("PUT", &format!("/ctl/{bad}"), b"x").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {xml}");
        assert_eq!(tags(&xml, "Code"), ["InvalidArgument"], "{bad}");
    }

    let (status, _, _) = app.send("PUT", "/ctl/tab%09nl%0Acr%0Dkey", b"x").await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, xml) = app.send("GET", "/ctl?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["tab\tnl\ncr&#13;key"]);
}

/// GET's `response-*` parameters set the headers they name, ranged or not,
/// as presigned download links use them (#177).
#[tokio::test]
async fn response_overrides_set_the_headers_they_name() {
    let app = App::new("overrides");
    app.send("PUT", "/demo/a.txt", b"0123456789").await;

    let uri = "/demo/a.txt?response-content-disposition=attachment%3B%20filename%3Dx.txt\
               &response-content-type=text/csv&X-Amz-Signature=abc";
    let (status, headers, body) = app.send("GET", uri, b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "0123456789"));
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=x.txt"
    );
    assert_eq!(headers[header::CONTENT_TYPE], "text/csv");

    let uri = "/demo/a.txt?response-content-language=fr\
               &response-expires=Thu,%2001%20Jan%202099%2000:00:00%20GMT\
               &response-cache-control=no-cache&response-content-encoding=identity";
    let (status, headers, body) = app
        .send_with("GET", uri, &[("range", "bytes=0-1")], b"")
        .await;
    assert_eq!((status, body.as_str()), (StatusCode::PARTIAL_CONTENT, "01"));
    assert_eq!(headers[header::CONTENT_LANGUAGE], "fr");
    assert_eq!(headers[header::EXPIRES], "Thu, 01 Jan 2099 00:00:00 GMT");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    assert_eq!(headers[header::CONTENT_ENCODING], "identity");
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain");

    let uri = "/demo/a.txt?response-content-type=a%0Ab";
    let (status, _, xml) = app.send("GET", uri, b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(tags(&xml, "Code"), ["InvalidArgument"]);
}

/// Conditional GETs, HEADs and PUTs, against the ETag (the capsule id) and
/// Last-Modified (#171).
#[tokio::test]
async fn conditional_requests_check_the_etag_and_dates() {
    let app = App::new("conditional");
    let (_, headers, _) = app.send("PUT", "/demo/a.txt", b"one").await;
    let etag = headers[header::ETAG].to_str().unwrap().to_string();
    let weak = format!("W/{etag}");
    let (_, headers, _) = app.send("HEAD", "/demo/a.txt", b"").await;
    let modified = headers[header::LAST_MODIFIED].to_str().unwrap().to_string();
    let other = "\"00000000-0000-0000-0000-000000000001\"";
    let listed = format!("{other}, {etag}");
    let past = "Sat, 01 Jan 2000 00:00:00 GMT";

    for method in ["GET", "HEAD"] {
        for (headers, expected) in [
            (
                vec![("if-none-match", etag.as_str())],
                StatusCode::NOT_MODIFIED,
            ),
            (
                vec![("if-none-match", weak.as_str())],
                StatusCode::NOT_MODIFIED,
            ),
            (vec![("if-none-match", "*")], StatusCode::NOT_MODIFIED),
            (vec![("if-none-match", other)], StatusCode::OK),
            (vec![("if-match", etag.as_str())], StatusCode::OK),
            (vec![("if-match", listed.as_str())], StatusCode::OK),
            (vec![("if-match", "*")], StatusCode::OK),
            (vec![("if-match", other)], StatusCode::PRECONDITION_FAILED),
            (
                vec![("if-match", weak.as_str())],
                StatusCode::PRECONDITION_FAILED,
            ),
            (
                vec![("if-modified-since", modified.as_str())],
                StatusCode::NOT_MODIFIED,
            ),
            (vec![("if-modified-since", past)], StatusCode::OK),
            (vec![("if-modified-since", "not a date")], StatusCode::OK),
            (
                vec![("if-unmodified-since", past)],
                StatusCode::PRECONDITION_FAILED,
            ),
            (
                vec![("if-unmodified-since", modified.as_str())],
                StatusCode::OK,
            ),
            // If-Match takes the place of If-Unmodified-Since, and
            // If-None-Match of If-Modified-Since.
            (
                vec![("if-match", etag.as_str()), ("if-unmodified-since", past)],
                StatusCode::OK,
            ),
            (
                vec![("if-none-match", other), ("if-modified-since", &modified)],
                StatusCode::OK,
            ),
        ] {
            let (status, _, _) = app.send_with(method, "/demo/a.txt", &headers, b"").await;
            assert_eq!(status, expected, "{method} {headers:?}");
        }
    }
    let (status, headers, body) = app
        .send_with("GET", "/demo/a.txt", &[("if-none-match", &etag)], b"")
        .await;
    assert_eq!((status, body.as_str()), (StatusCode::NOT_MODIFIED, ""));
    assert_eq!(headers[header::ETAG], etag.as_str());
    let (_, _, xml) = app
        .send_with("GET", "/demo/a.txt", &[("if-match", other)], b"")
        .await;
    assert_eq!(tags(&xml, "Code"), ["PreconditionFailed"]);

    // PUT: If-None-Match: * creates only, If-Match swaps only the ETag
    // named, and a PUT that fails stores nothing.
    let put = |key: &'static str, headers: Vec<(&'static str, String)>, body: &'static str| {
        let app = &app;
        async move {
            let headers: Vec<(&str, &str)> =
                headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
            app.send_with("PUT", key, &headers, body.as_bytes()).await
        }
    };
    let failed = StatusCode::PRECONDITION_FAILED;
    let (status, _, xml) = put("/demo/a.txt", vec![("if-none-match", "*".into())], "two").await;
    assert_eq!(status, failed, "{xml}");
    assert_eq!(tags(&xml, "Code"), ["PreconditionFailed"]);
    let (status, _, _) = put("/demo/a.txt", vec![("if-match", other.into())], "two").await;
    assert_eq!(status, failed);
    let (status, _, _) = put(
        "/demo/a.txt",
        vec![("if-unmodified-since", past.into())],
        "two",
    )
    .await;
    assert_eq!(status, failed);
    let (status, _, _) = put("/demo/new.txt", vec![("if-match", etag.clone())], "two").await;
    assert_eq!(status, failed);
    let (status, _, body) = app.send("GET", "/demo/a.txt", b"").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "one"));

    let (status, headers, _) = put("/demo/a.txt", vec![("if-match", etag.clone())], "two").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(headers[header::ETAG], etag.as_str());
    let (status, _, _) = put("/demo/a.txt", vec![("if-match", etag.clone())], "three").await;
    assert_eq!(status, failed);
    let (status, _, _) = put("/demo/new.txt", vec![("if-none-match", "*".into())], "new").await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, xml) = app.send("GET", "/demo?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a.txt", "new.txt"]);
    let (_, _, body) = app.send("GET", "/demo/a.txt", b"").await;
    assert_eq!(body, "two");
}

/// As in S3, a PUT into a bucket that isn't there is `NoSuchBucket`, so a
/// typo in a bucket name can't create a bucket (#164). The body isn't
/// stored.
#[tokio::test]
async fn a_put_into_a_missing_bucket_creates_nothing() {
    let app = App::new("missing_bucket");
    let log = app.dir.join("space.nvram");
    let before = std::fs::metadata(&log).map_or(0, |m| m.len());

    let (status, _, xml) = app.send("PUT", "/dmeo/a.txt", b"typo").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{xml}");
    assert_eq!(tags(&xml, "Code"), ["NoSuchBucket"]);
    assert_eq!(tags(&xml, "Resource"), ["dmeo"]);
    assert_eq!(
        app.send("HEAD", "/dmeo", b"").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(std::fs::metadata(&log).map_or(0, |m| m.len()), before);

    let (status, _, _) = app.send("PUT", "/demo/a.txt", b"fine").await;
    assert_eq!(status, StatusCode::OK);
}

/// A new bucket's name follows S3's rules (#164).
#[tokio::test]
async fn bucket_names_follow_s3_rules() {
    let app = App::new("bucket_names");
    let long = "a".repeat(64);
    for bad in [
        "UPPER",
        "with%20space",
        "under_score",
        "%F0%9F%9A%80",
        "a..b",
        "-lead",
        "trail-",
        ".dot",
        "dot.",
        "-",
        "x",
        "ab",
        &long,
        "192.168.5.4",
    ] {
        let (status, _, xml) = app.send("PUT", &format!("/{bad}"), b"").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {xml}");
        assert_eq!(tags(&xml, "Code"), ["InvalidBucketName"], "{bad}");
        assert!(xml.contains("3 to 63 lowercase letters"), "{xml}");
        let (status, _, _) = app.send("HEAD", &format!("/{bad}"), b"").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
    }
    let longest = "a".repeat(63);
    for good in [
        "abc",
        "a.b-c",
        "0-9",
        "1.2.3",
        "1.2.3.4x",
        "my--bucket",
        &longest,
    ] {
        let (status, _, xml) = app.send("PUT", &format!("/{good}"), b"").await;
        assert_eq!(status, StatusCode::OK, "{good}: {xml}");
        assert_eq!(
            app.send("HEAD", &format!("/{good}"), b"").await.0,
            StatusCode::OK
        );
    }
}

/// A store an older build wrote may hold buckets these rules refuse. They
/// keep working, and creating one again is still not an error.
#[tokio::test]
async fn buckets_named_before_the_rules_keep_working() {
    let app = App::with_store("old_names", |registry| {
        // The namespace the S3 view keeps a bucket's keys in.
        registry.create_namespace("s3:Old_Bucket").unwrap();
    });
    assert_eq!(app.send("HEAD", "/Old_Bucket", b"").await.0, StatusCode::OK);
    assert_eq!(app.send("PUT", "/Old_Bucket", b"").await.0, StatusCode::OK);
    assert_eq!(
        app.send("PUT", "/Old_Bucket/a.txt", b"kept").await.0,
        StatusCode::OK
    );
    let (_, _, body) = app.send("GET", "/Old_Bucket/a.txt", b"").await;
    assert_eq!(body, "kept");
    let (_, _, xml) = app.send("GET", "/Old_Bucket?list-type=2", b"").await;
    assert_eq!(tags(&xml, "Key"), ["a.txt"]);
    let (status, _, _) = app.send("DELETE", "/Old_Bucket/a.txt", b"").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// The health check is at `/-/health`, where no bucket can be, so a bucket
/// named `health` lists like any other (#164).
#[tokio::test]
async fn the_health_check_hides_no_bucket() {
    let app = App::new("health");
    let (status, _, body) = app.send("GET", "/-/health", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("healthy"), "{body}");

    assert_eq!(app.send("PUT", "/health", b"").await.0, StatusCode::OK);
    app.send("PUT", "/health/a.txt", b"x").await;
    for uri in ["/health", "/health?list-type=2"] {
        let (status, _, xml) = app.send("GET", uri, b"").await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(tags(&xml, "Key"), ["a.txt"], "{uri}: {xml}");
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

/// A PUT stores Content-Type, the other representation headers and
/// `x-amz-meta-*`, and GET and HEAD return them (#163).
#[tokio::test]
async fn put_stores_the_headers_get_and_head_return() {
    let app = App::new("metadata");
    let sent = [
        ("content-type", "image/x-test"),
        ("cache-control", "max-age=60"),
        ("content-disposition", "attachment; filename=\"img.JPG\""),
        ("content-language", "en"),
        ("x-amz-meta-owner", "qa"),
        ("X-Amz-Meta-Mixed-Case", "kept"),
        ("x-amz-meta-tag", "a"),
        ("x-amz-meta-tag", "b"),
        ("x-unrelated", "dropped"),
    ];
    let (status, _, _) = app
        .send_with("PUT", "/demo/img.JPG", &sent, b"\xff\xd8")
        .await;
    assert_eq!(status, StatusCode::OK);

    let expected = [
        ("content-type", "image/x-test"),
        ("cache-control", "max-age=60"),
        ("content-disposition", "attachment; filename=\"img.JPG\""),
        ("content-language", "en"),
        ("x-amz-meta-owner", "qa"),
        ("x-amz-meta-mixed-case", "kept"),
        ("x-amz-meta-tag", "a,b"),
    ];
    for (method, range) in [("HEAD", None), ("GET", None), ("GET", Some("bytes=0-0"))] {
        let extra: Vec<(&str, &str)> = range.map(|r| ("range", r)).into_iter().collect();
        let (_, headers, _) = app.send_with(method, "/demo/img.JPG", &extra, b"").await;
        for (name, value) in expected {
            assert_eq!(
                header(&headers, name),
                Some(value),
                "{method} {range:?} {name}"
            );
        }
        assert_eq!(header(&headers, "x-unrelated"), None);
    }

    // A response-* parameter still sets the header it names.
    let (_, headers, _) = app
        .send("GET", "/demo/img.JPG?response-content-type=text/plain", b"")
        .await;
    assert_eq!(header(&headers, "content-type"), Some("text/plain"));
    assert_eq!(header(&headers, "x-amz-meta-owner"), Some("qa"));

    // An overwrite replaces the metadata. With no type sent, it's guessed
    // from the extension, whatever its case.
    app.send("PUT", "/demo/img.JPG", b"\xff\xd8").await;
    let (_, headers, _) = app.send("HEAD", "/demo/img.JPG", b"").await;
    assert_eq!(header(&headers, "content-type"), Some("image/jpeg"));
    assert_eq!(header(&headers, "x-amz-meta-owner"), None);
    assert_eq!(header(&headers, "cache-control"), None);
}

/// Without a Content-Type, the type comes from the key's extension.
#[tokio::test]
async fn an_untyped_object_gets_a_type_from_its_extension() {
    let app = App::new("guessed");
    for (key, expected) in [
        ("a.SVG", "image/svg+xml"),
        ("table.csv", "text/csv"),
        ("app.wasm", "application/wasm"),
        ("logs.gz", "application/gzip"),
        ("clip.Mp4", "video/mp4"),
        ("page.HTML", "text/html"),
        ("noext", "application/octet-stream"),
        ("dir.v2/file", "application/octet-stream"),
    ] {
        app.send("PUT", &format!("/demo/{key}"), b"x").await;
        let (_, headers, _) = app.send("HEAD", &format!("/demo/{key}"), b"").await;
        assert_eq!(header(&headers, "content-type"), Some(expected), "{key}");
    }
}

/// As in S3, user metadata is limited to 2 KB: the names, without the
/// prefix, and the values. A PUT over it stores nothing.
#[tokio::test]
async fn user_metadata_over_2_kb_is_refused() {
    let app = App::new("metadata_size");
    let fits = "v".repeat(2048 - "big".len());
    let (status, _, xml) = app
        .send_with("PUT", "/demo/fits", &[("x-amz-meta-big", &fits)], b"x")
        .await;
    assert_eq!(status, StatusCode::OK, "{xml}");

    let log = app.dir.join("space.nvram");
    let before = std::fs::metadata(&log).unwrap().len();
    let over = format!("{fits}v");
    let (status, _, xml) = app
        .send_with("PUT", "/demo/over", &[("x-amz-meta-big", &over)], b"x")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(tags(&xml, "Code"), ["MetadataTooLarge"]);
    assert_eq!(std::fs::metadata(&log).unwrap().len(), before);
    assert_eq!(
        app.send("HEAD", "/demo/over", b"").await.0,
        StatusCode::NOT_FOUND
    );
}

/// A stored header value must be ASCII, as S3 requires.
#[tokio::test]
async fn a_header_value_that_isnt_ascii_is_refused() {
    let app = App::new("metadata_ascii");
    let request = Request::put("/demo/a.txt")
        .header(
            "x-amz-meta-name",
            axum::http::HeaderValue::from_bytes(b"caf\xc3\xa9").unwrap(),
        )
        .body(Body::from("x"))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        app.send("HEAD", "/demo/a.txt", b"").await.0,
        StatusCode::NOT_FOUND
    );
}

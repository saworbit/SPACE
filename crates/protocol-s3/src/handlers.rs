use anyhow::Error;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use common::CapsuleId;
use futures::TryStreamExt;
use serde::Deserialize;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tracing::{error, info};

use crate::{
    ByteRange, ListPage, ListQuery, ObjectMapping, Preconditions, PutOptions, S3Error, S3Result,
    S3View, STORED_HEADERS, USER_METADATA_PREFIX,
};

pub type AppState = Arc<S3View>;

/// S3 caps a listing page at 1000 entries.
const MAX_KEYS: usize = 1000;

/// Error responses use the S3 XML error body, so clients can read the code.
impl IntoResponse for S3Error {
    fn into_response(self) -> Response {
        let (status, code, resource) = match &self {
            S3Error::NoSuchBucket(bucket) => {
                (StatusCode::NOT_FOUND, "NoSuchBucket", bucket.as_str())
            }
            S3Error::NoSuchKey(key) => (StatusCode::NOT_FOUND, "NoSuchKey", key.as_str()),
            S3Error::InvalidBucketName(bucket) => (
                StatusCode::BAD_REQUEST,
                "InvalidBucketName",
                bucket.as_str(),
            ),
            S3Error::InvalidArgument(_) => (StatusCode::BAD_REQUEST, "InvalidArgument", ""),
            S3Error::MetadataTooLarge => (StatusCode::BAD_REQUEST, "MetadataTooLarge", ""),
            S3Error::InvalidRange(_) => (StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange", ""),
            S3Error::NotImplemented(_) => (StatusCode::NOT_IMPLEMENTED, "NotImplemented", ""),
            S3Error::PreconditionFailed => {
                (StatusCode::PRECONDITION_FAILED, "PreconditionFailed", "")
            }
            // A 304 carries the validators and no body (RFC 9110 §15.4.5).
            S3Error::NotModified(meta) => {
                let mut headers = HeaderMap::new();
                insert(&mut headers, header::ETAG, etag(meta.capsule_id));
                insert(
                    &mut headers,
                    header::LAST_MODIFIED,
                    http_date(meta.created_at),
                );
                return (StatusCode::NOT_MODIFIED, headers).into_response();
            }
            S3Error::Internal(err) => {
                error!("request failed: {err:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "InternalError", "")
            }
        };
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>{code}</Code><Message>{}</Message><Resource>{}</Resource></Error>"#,
            xml_escape(&self.to_string()),
            xml_escape(resource)
        );
        let mut response =
            (status, [(header::CONTENT_TYPE, "application/xml")], body).into_response();
        if let S3Error::InvalidRange(size) = self {
            insert(
                response.headers_mut(),
                header::CONTENT_RANGE,
                format!("bytes */{size}"),
            );
        }
        response
    }
}

/// Query parameters any request may carry, which the server ignores:
/// presigned-URL authentication, SigV4 and SigV2. Credentials aren't
/// checked.
const AUTH_PARAMS: &[&str] = &[
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
    "X-Amz-Security-Token",
    "AWSAccessKeyId",
    "Signature",
    "Expires",
];

/// The ListObjects and ListObjectsV2 parameters `list_objects` reads, and
/// `fetch-owner`, which asks for owners there are none of.
const LIST_PARAMS: &[&str] = &[
    "list-type",
    "prefix",
    "delimiter",
    "max-keys",
    "continuation-token",
    "start-after",
    "marker",
    "encoding-type",
    "fetch-owner",
];

/// GetObject's `response-*` parameters, and the header each one sets in
/// place of the stored one (#177).
const RESPONSE_OVERRIDES: [(&str, &str); 6] = [
    ("response-content-type", "content-type"),
    ("response-content-language", "content-language"),
    ("response-expires", "expires"),
    ("response-cache-control", "cache-control"),
    ("response-content-disposition", "content-disposition"),
    ("response-content-encoding", "content-encoding"),
];

/// A request's query parameters, decoded, in order.
type RawQuery = Query<Vec<(String, String)>>;

/// The headers a GET's `response-*` parameters set. A value that can't be
/// a header is refused before anything is read.
fn response_overrides(query: &[(String, String)]) -> S3Result<Vec<(HeaderName, HeaderValue)>> {
    let mut overrides = Vec::new();
    for (param, name) in RESPONSE_OVERRIDES {
        if let Some((_, value)) = query.iter().rev().find(|(p, _)| p == param) {
            let value = HeaderValue::from_str(value).map_err(|_| {
                S3Error::InvalidArgument(format!("'{param}' is not a valid header value"))
            })?;
            overrides.push((HeaderName::from_static(name), value));
        }
    }
    Ok(overrides)
}

/// A request's conditional headers (#171). Repeated `If-Match` and
/// `If-None-Match` lines are one list. A date that doesn't parse is
/// ignored, as RFC 9110 says.
fn preconditions(headers: &HeaderMap) -> Preconditions {
    let list = |name: HeaderName| {
        let values: Vec<&str> = headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        (!values.is_empty()).then(|| values.join(","))
    };
    let date = |name: HeaderName| {
        let value = headers.get(name)?.to_str().ok()?;
        let time = httpdate::parse_http_date(value).ok()?;
        Some(time.duration_since(UNIX_EPOCH).ok()?.as_secs())
    };
    Preconditions {
        if_match: list(header::IF_MATCH),
        if_none_match: list(header::IF_NONE_MATCH),
        if_modified_since: date(header::IF_MODIFIED_SINCE),
        if_unmodified_since: date(header::IF_UNMODIFIED_SINCE),
    }
}

/// Refuse a request whose query asks for something its handler doesn't do
/// (#156). S3 picks the operation from the query as well as the method:
/// `PUT ?tagging` is PutObjectTagging, and run as a PutObject it would
/// replace the object with the tagging XML. So every parameter must be one
/// the handler reads (`allowed`) or authentication, and the SDKs' `x-id`
/// operation tag must name one of `operations`.
fn check_query(params: &[(String, String)], allowed: &[&str], operations: &[&str]) -> S3Result<()> {
    for (name, value) in params {
        if name == "x-id" {
            if operations.contains(&value.as_str()) {
                continue;
            }
            return Err(S3Error::NotImplemented(format!(
                "{value} is not implemented"
            )));
        }
        if !allowed.contains(&name.as_str())
            && !AUTH_PARAMS.iter().any(|p| p.eq_ignore_ascii_case(name))
        {
            return Err(S3Error::NotImplemented(format!(
                "the '{name}' query parameter is not implemented"
            )));
        }
    }
    Ok(())
}

/// Refuse a PUT whose headers make it something other than a plain
/// PutObject (#156): CopyObject, which would otherwise store the empty body
/// as the copy, and an `aws-chunked` body, which would be stored with its
/// chunk framing.
fn check_put_headers(headers: &HeaderMap) -> S3Result<()> {
    if headers.contains_key("x-amz-copy-source") {
        return Err(S3Error::NotImplemented(
            "CopyObject is not implemented".to_string(),
        ));
    }
    let header_has = |name: &str, needle: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.to_ascii_lowercase().contains(needle))
    };
    if header_has("x-amz-content-sha256", "streaming-")
        || header_has(header::CONTENT_ENCODING.as_str(), "aws-chunked")
    {
        return Err(S3Error::NotImplemented(
            "aws-chunked uploads are not implemented; send the body unencoded".to_string(),
        ));
    }
    Ok(())
}

/// The headers a PUT stores with the object (#163): [`STORED_HEADERS`] and
/// `x-amz-meta-*`, sorted by name. A header sent more than once is stored
/// once, with its values joined by commas.
fn stored_metadata(headers: &HeaderMap) -> S3Result<Vec<(String, String)>> {
    let mut metadata = Vec::new();
    for name in headers.keys() {
        let name = name.as_str();
        if !STORED_HEADERS.contains(&name) && !name.starts_with(USER_METADATA_PREFIX) {
            continue;
        }
        let values = headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| S3Error::InvalidArgument(format!("the {name} header must be ASCII")))?;
        metadata.push((name.to_string(), values.join(",")));
    }
    metadata.sort();
    Ok(metadata)
}

/// Requests no route serves, such as ListBuckets (`GET /`), DeleteBucket,
/// DeleteObjects and multipart uploads, get an S3 error a client can read
/// instead of an empty 404 or 405 (#156).
pub async fn not_implemented(method: Method, uri: Uri) -> Response {
    S3Error::NotImplemented(format!("{method} {} is not implemented", uri.path())).into_response()
}

/// PUT /{bucket}/{key}
pub async fn put_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(query): RawQuery,
    request_headers: HeaderMap,
    body: Body,
) -> Response {
    info!("PUT (streaming) /{}/{}", bucket, key);
    if let Err(e) =
        check_query(&query, &[], &["PutObject"]).and_then(|()| check_put_headers(&request_headers))
    {
        return e.into_response();
    }

    let options = match stored_metadata(&request_headers) {
        Ok(metadata) => PutOptions {
            conditions: preconditions(&request_headers),
            metadata,
        },
        Err(e) => return e.into_response(),
    };

    // Convert Axum Body into a standard futures::Stream
    let stream = body.into_data_stream().map_err(Error::from).into_stream();

    match s3.put_object_with(&bucket, &key, stream, &options).await {
        Ok(capsule_id) => {
            info!(
                "Created capsule {} for {}/{}",
                capsule_id.as_uuid(),
                bucket,
                key
            );
            (StatusCode::OK, [(header::ETAG, etag(capsule_id))]).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// GET /{bucket}/{key}, honouring a single `Range: bytes=` range (#99), the
/// conditional headers (#171) and the `response-*` overrides (#177).
pub async fn get_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(query): RawQuery,
    request_headers: HeaderMap,
) -> Response {
    info!("GET /{}/{}", bucket, key);
    let allowed = RESPONSE_OVERRIDES.map(|(param, _)| param);
    let overrides = match check_query(&query, &allowed, &["GetObject"])
        .and_then(|()| response_overrides(&query))
    {
        Ok(overrides) => overrides,
        Err(e) => return e.into_response(),
    };

    let range = request_headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(ByteRange::parse);
    let conditions = preconditions(&request_headers);
    match s3.get_object_if(&bucket, &key, range, &conditions).await {
        Ok(object) => {
            let mut headers = object_headers(&object.meta);
            let status = match object.range {
                Some((first, last)) => {
                    let size = object.meta.size;
                    insert(
                        &mut headers,
                        header::CONTENT_LENGTH,
                        (last - first + 1).to_string(),
                    );
                    insert(
                        &mut headers,
                        header::CONTENT_RANGE,
                        format!("bytes {first}-{last}/{size}"),
                    );
                    StatusCode::PARTIAL_CONTENT
                }
                None => StatusCode::OK,
            };
            for (name, value) in overrides {
                headers.insert(name, value);
            }
            (status, headers, Body::from_stream(object.body)).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// HEAD /{bucket}/{key}, honouring the conditional headers (#171).
pub async fn head_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(query): RawQuery,
    request_headers: HeaderMap,
) -> Response {
    info!("HEAD /{}/{}", bucket, key);
    if let Err(e) = check_query(&query, &[], &["HeadObject"]) {
        return e.into_response();
    }

    let head = s3.head_object(&bucket, &key).and_then(|mapping| {
        preconditions(&request_headers).check_read(&mapping)?;
        Ok(mapping)
    });
    match head {
        Ok(mapping) => (StatusCode::OK, object_headers(&mapping)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// DELETE /{bucket}/{key}
pub async fn delete_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(query): RawQuery,
) -> Response {
    info!("DELETE /{}/{}", bucket, key);
    if let Err(e) = check_query(&query, &[], &["DeleteObject"]) {
        return e.into_response();
    }

    match s3.delete_object(&bucket, &key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

/// PUT /{bucket}
pub async fn create_bucket(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): RawQuery,
) -> Response {
    info!("CREATE BUCKET /{}", bucket);
    // `PUT /{bucket}?versioning` and the like configure a bucket; they
    // mustn't create one.
    if let Err(e) = check_query(&query, &[], &["CreateBucket"]) {
        return e.into_response();
    }

    match s3.create_bucket(&bucket) {
        Ok(()) => (StatusCode::OK, [(header::LOCATION, format!("/{bucket}"))]).into_response(),
        Err(e) => e.into_response(),
    }
}

/// HEAD /{bucket}
pub async fn head_bucket(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): RawQuery,
) -> Response {
    if let Err(e) = check_query(&query, &[], &["HeadBucket"]) {
        return e.into_response();
    }
    match s3.head_bucket(&bucket) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => e.into_response(),
    }
}

/// ListObjects query parameters. `list-type=2` selects ListObjectsV2.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ListParams {
    list_type: Option<String>,
    prefix: Option<String>,
    delimiter: Option<String>,
    max_keys: Option<String>,
    continuation_token: Option<String>,
    start_after: Option<String>,
    marker: Option<String>,
    encoding_type: Option<String>,
}

/// GET /{bucket}: ListObjects (V1) or ListObjectsV2, as `ListBucketResult`
/// XML (#89), or GetBucketLocation with `?location`.
pub async fn list_objects(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(params): Query<ListParams>,
    Query(query): RawQuery,
) -> Response {
    if query.iter().any(|(name, _)| name == "location") {
        return match check_query(&query, &["location"], &["GetBucketLocation"]) {
            Ok(()) => bucket_location(&s3, &bucket),
            Err(e) => e.into_response(),
        };
    }
    info!("LIST /{}", bucket);
    if let Err(e) = check_query(&query, LIST_PARAMS, &["ListObjects", "ListObjectsV2"]) {
        return e.into_response();
    }

    let v2 = params.list_type.as_deref() == Some("2");
    let max_keys = match params.max_keys.as_deref().map(str::parse::<usize>) {
        None => MAX_KEYS,
        Some(Ok(n)) => n.min(MAX_KEYS),
        Some(Err(_)) => {
            return S3Error::InvalidArgument("max-keys must be a non-negative integer".into())
                .into_response()
        }
    };
    let start_after = if v2 {
        match params.continuation_token.as_deref() {
            Some(token) => match decode_token(token) {
                Some(key) => Some(key),
                None => {
                    return S3Error::InvalidArgument("The continuation token is not valid".into())
                        .into_response()
                }
            },
            None => params.start_after.clone(),
        }
    } else {
        params.marker.clone()
    };
    let query = ListQuery {
        prefix: params.prefix.clone().unwrap_or_default(),
        delimiter: params.delimiter.clone(),
        start_after,
        max_keys,
    };

    match s3.list_objects(&bucket, &query) {
        Ok(page) => {
            info!("Listed {} objects in {}", page.objects.len(), bucket);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml")],
                list_xml(&bucket, &query, &params, &page, v2),
            )
                .into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// GetBucketLocation, which some clients, such as the MinIO SDK, call to
/// pick a signing region. An empty constraint means us-east-1, the region
/// they assume for a custom endpoint anyway.
fn bucket_location(s3: &S3View, bucket: &str) -> Response {
    match s3.head_bucket(bucket) {
        Ok(()) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/xml")],
            r#"<?xml version="1.0" encoding="UTF-8"?><LocationConstraint xmlns="http://s3.amazonaws.com/doc/2006-03-01/"></LocationConstraint>"#,
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// Health check endpoint
pub async fn health_check() -> Response {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "SPACE S3 Protocol View"
    }))
    .into_response()
}

fn list_xml(
    bucket: &str,
    query: &ListQuery,
    params: &ListParams,
    page: &ListPage,
    v2: bool,
) -> String {
    // With `encoding-type=url`, keys and prefixes are percent-encoded so any
    // key fits in XML. Clients decode them when the response says so.
    let url = params.encoding_type.as_deref() == Some("url");
    let enc = |s: &str| if url { url_encode(s) } else { s.to_string() };

    let mut x = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    x.push_str(r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#);
    element(&mut x, "Name", bucket);
    element(&mut x, "Prefix", &enc(&query.prefix));
    if let Some(delimiter) = &query.delimiter {
        element(&mut x, "Delimiter", &enc(delimiter));
    }
    element(&mut x, "MaxKeys", &query.max_keys.to_string());
    if url {
        element(&mut x, "EncodingType", "url");
    }
    let truncated = page.next_start_after.is_some();
    element(
        &mut x,
        "IsTruncated",
        if truncated { "true" } else { "false" },
    );
    if v2 {
        let count = page.objects.len() + page.common_prefixes.len();
        element(&mut x, "KeyCount", &count.to_string());
        if let Some(token) = &params.continuation_token {
            element(&mut x, "ContinuationToken", token);
        }
        if let Some(start_after) = &params.start_after {
            element(&mut x, "StartAfter", &enc(start_after));
        }
        if let Some(next) = &page.next_start_after {
            element(&mut x, "NextContinuationToken", &encode_token(next));
        }
    } else {
        element(
            &mut x,
            "Marker",
            &enc(params.marker.as_deref().unwrap_or("")),
        );
        if let Some(next) = &page.next_start_after {
            element(&mut x, "NextMarker", &enc(next));
        }
    }
    for object in &page.objects {
        x.push_str("<Contents>");
        element(&mut x, "Key", &enc(&object.key));
        element(&mut x, "LastModified", &iso8601(object.created_at));
        element(&mut x, "ETag", &etag(object.capsule_id));
        element(&mut x, "Size", &object.size.to_string());
        element(&mut x, "StorageClass", "STANDARD");
        x.push_str("</Contents>");
    }
    for prefix in &page.common_prefixes {
        x.push_str("<CommonPrefixes>");
        element(&mut x, "Prefix", &enc(prefix));
        x.push_str("</CommonPrefixes>");
    }
    x.push_str("</ListBucketResult>");
    x
}

fn element(out: &mut String, name: &str, text: &str) {
    let _ = write!(out, "<{name}>{}</{name}>", xml_escape(text));
}

fn object_headers(meta: &ObjectMapping) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert(&mut headers, header::CONTENT_LENGTH, meta.size.to_string());
    insert(
        &mut headers,
        header::CONTENT_TYPE,
        meta.content_type.clone(),
    );
    for (name, value) in &meta.metadata {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
            insert(&mut headers, name, value.clone());
        }
    }
    insert(&mut headers, header::ETAG, etag(meta.capsule_id));
    insert(
        &mut headers,
        header::LAST_MODIFIED,
        http_date(meta.created_at),
    );
    insert(&mut headers, header::ACCEPT_RANGES, "bytes".to_string());
    headers
}

fn http_date(secs: u64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Every value set here is built from ASCII, so it is always a valid header.
fn insert(headers: &mut HeaderMap, name: HeaderName, value: String) {
    if let Ok(value) = HeaderValue::try_from(value) {
        headers.insert(name, value);
    }
}

/// The ETag is the capsule ID, not an MD5 of the body.
fn etag(id: CapsuleId) -> String {
    format!("\"{}\"", id.as_uuid())
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // A parser would read a bare CR back as LF, changing the key.
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// Percent-encoding for `encoding-type=url`. Unreserved characters and '/'
/// are kept.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Continuation tokens are the hex of the last key returned, so they are
/// opaque and safe in XML and in a query string.
fn encode_token(key: &str) -> String {
    key.bytes().map(|b| format!("{b:02x}")).collect()
}

fn decode_token(token: &str) -> Option<String> {
    if !token.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..token.len())
        .step_by(2)
        .map(|i| {
            token
                .get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// Seconds since the Unix epoch as an ISO 8601 UTC time, the format S3
/// listings use for `LastModified`.
fn iso8601(secs: u64) -> String {
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formats_utc_times() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn continuation_tokens_roundtrip_and_reject_garbage() {
        let key = "a/b c&<d>.txt";
        assert_eq!(decode_token(&encode_token(key)).as_deref(), Some(key));
        assert_eq!(decode_token("abc"), None);
        assert_eq!(decode_token("zz"), None);
    }

    #[test]
    fn url_encoding_keeps_slashes_and_unreserved_characters() {
        assert_eq!(url_encode("dir/a b+c~.txt"), "dir/a%20b%2Bc~.txt");
    }
}

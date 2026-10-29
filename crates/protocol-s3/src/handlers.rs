use anyhow::Error;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
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

use crate::{ByteRange, ListPage, ListQuery, ObjectMapping, S3Error, S3View};

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
            S3Error::InvalidRange(_) => (StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange", ""),
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

/// PUT /{bucket}/{key}
pub async fn put_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    body: Body,
) -> Response {
    info!("PUT (streaming) /{}/{}", bucket, key);

    // Convert Axum Body into a standard futures::Stream
    let stream = body.into_data_stream().map_err(Error::from).into_stream();

    match s3.put_object(&bucket, &key, stream).await {
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

/// GET /{bucket}/{key}, honouring a single `Range: bytes=` range (#99).
pub async fn get_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    request_headers: HeaderMap,
) -> Response {
    info!("GET /{}/{}", bucket, key);

    let range = request_headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(ByteRange::parse);
    match s3.get_object_range(&bucket, &key, range).await {
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
            (status, headers, Body::from_stream(object.body)).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// HEAD /{bucket}/{key}
pub async fn head_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    info!("HEAD /{}/{}", bucket, key);

    match s3.head_object(&bucket, &key) {
        Ok(mapping) => (StatusCode::OK, object_headers(&mapping)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// DELETE /{bucket}/{key}
pub async fn delete_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    info!("DELETE /{}/{}", bucket, key);

    match s3.delete_object(&bucket, &key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

/// PUT /{bucket}
pub async fn create_bucket(State(s3): State<AppState>, Path(bucket): Path<String>) -> Response {
    info!("CREATE BUCKET /{}", bucket);

    match s3.create_bucket(&bucket) {
        Ok(()) => (StatusCode::OK, [(header::LOCATION, format!("/{bucket}"))]).into_response(),
        Err(e) => e.into_response(),
    }
}

/// HEAD /{bucket}
pub async fn head_bucket(State(s3): State<AppState>, Path(bucket): Path<String>) -> Response {
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
/// XML (#89).
pub async fn list_objects(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(params): Query<ListParams>,
) -> Response {
    info!("LIST /{}", bucket);

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
    insert(&mut headers, header::ETAG, etag(meta.capsule_id));
    insert(
        &mut headers,
        header::LAST_MODIFIED,
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(meta.created_at)),
    );
    insert(&mut headers, header::ACCEPT_RANGES, "bytes".to_string());
    headers
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

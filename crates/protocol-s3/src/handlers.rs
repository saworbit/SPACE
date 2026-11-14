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

use crate::checksum::{self, Algorithm};
use crate::{
    ByteRange, ChunkedBody, ListPage, ListQuery, ObjectMapping, Preconditions, PutOptions, S3Error,
    S3Result, S3View, STORED_HEADERS, USER_METADATA_PREFIX,
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
            S3Error::BucketNotEmpty(bucket) => {
                (StatusCode::CONFLICT, "BucketNotEmpty", bucket.as_str())
            }
            S3Error::AccessDenied(_) => (StatusCode::FORBIDDEN, "AccessDenied", ""),
            S3Error::BadDigest(expected) if expected.header == "x-amz-content-sha256" => {
                (StatusCode::BAD_REQUEST, "XAmzContentSHA256Mismatch", "")
            }
            S3Error::BadDigest(_) => (StatusCode::BAD_REQUEST, "BadDigest", ""),
            S3Error::InvalidDigest(_) => (StatusCode::BAD_REQUEST, "InvalidDigest", ""),
            S3Error::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "InvalidRequest", ""),
            S3Error::InvalidBucketName(bucket) => (
                StatusCode::BAD_REQUEST,
                "InvalidBucketName",
                bucket.as_str(),
            ),
            S3Error::InvalidArgument(_) => (StatusCode::BAD_REQUEST, "InvalidArgument", ""),
            S3Error::MalformedXML(_) => (StatusCode::BAD_REQUEST, "MalformedXML", ""),
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
/// as the copy.
fn check_put_headers(headers: &HeaderMap) -> S3Result<()> {
    if headers.contains_key("x-amz-copy-source") {
        return Err(S3Error::NotImplemented(
            "CopyObject is not implemented".to_string(),
        ));
    }
    // Headers that ask S3 to protect, encrypt or place the object (#196).
    // The server can't do what they ask, so they're refused rather than
    // answered with a 200 that says it did.
    for name in headers.keys() {
        let name = name.as_str();
        let refused = if name.starts_with("x-amz-object-lock-") {
            Some("Object Lock is not implemented")
        } else if name.starts_with("x-amz-server-side-encryption") {
            Some(
                "server-side encryption options are not implemented; objects are encrypted \
                 when the server has SPACE_MASTER_KEY",
            )
        } else if name == "x-amz-tagging" {
            Some("object tagging is not implemented")
        } else if name.starts_with("x-amz-grant-")
            || (name == "x-amz-acl" && !header_is(headers, name, "private"))
        {
            Some("ACLs are not implemented")
        } else if name == "x-amz-storage-class" && !header_is(headers, name, "STANDARD") {
            Some("storage classes other than STANDARD are not implemented")
        } else if name == "x-amz-website-redirect-location" {
            Some("website redirects are not implemented")
        } else {
            None
        };
        if let Some(why) = refused {
            return Err(S3Error::NotImplemented(format!("{why} ({name})")));
        }
    }
    Ok(())
}

/// Whether a `Content-Encoding` token is `aws-chunked`, the framing of a
/// streamed upload rather than an encoding of the object.
fn is_aws_chunked(token: &str) -> bool {
    token.trim().eq_ignore_ascii_case("aws-chunked")
}

/// Whether `x-amz-content-sha256` says the body is streamed in
/// `aws-chunked` framing: `STREAMING-AWS4-HMAC-SHA256-PAYLOAD`,
/// `STREAMING-UNSIGNED-PAYLOAD-TRAILER` and the like.
fn is_streaming(sha256: &str) -> bool {
    sha256
        .get(..10)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("STREAMING-"))
}

/// How a PUT body in `aws-chunked` framing is to be decoded (#178), or
/// `None` for a plain body. The framing is signalled by `Content-Encoding:
/// aws-chunked` or a `STREAMING-*` `x-amz-content-sha256`.
fn chunked_body(headers: &HeaderMap) -> S3Result<Option<ChunkedBody>> {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let encoded = headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.split(',').any(is_aws_chunked));
    if !encoded && !value("x-amz-content-sha256").is_some_and(is_streaming) {
        return Ok(None);
    }
    let decoded_length = match value("x-amz-decoded-content-length") {
        None => None,
        Some(n) => Some(n.trim().parse::<u64>().map_err(|_| {
            S3Error::InvalidArgument("x-amz-decoded-content-length must be a number".into())
        })?),
    };
    let mut trailer = Vec::new();
    for name in value("x-amz-trailer")
        .into_iter()
        .flat_map(|v| v.split(','))
    {
        let name = name.trim().to_ascii_lowercase();
        let algorithm = name
            .strip_prefix("x-amz-checksum-")
            .and_then(Algorithm::from_checksum_name)
            .ok_or_else(|| {
                S3Error::NotImplemented(format!("the {name} trailer is not implemented"))
            })?;
        trailer.push(algorithm);
    }
    Ok(Some(ChunkedBody {
        decoded_length,
        trailer,
    }))
}

/// Whether every `name` header is `value`, ignoring case.
fn header_is(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers.get_all(name).iter().all(|v| {
        v.to_str()
            .is_ok_and(|v| v.trim().eq_ignore_ascii_case(value))
    })
}

/// The digests a PUT's headers say its body has (#196): `Content-MD5`, the
/// `x-amz-checksum-*` headers, and a hex `x-amz-content-sha256`. A digest
/// that doesn't parse is refused, and so is a checksum algorithm the
/// server doesn't compute, rather than ignored.
fn expected_digests(headers: &HeaderMap) -> S3Result<Vec<checksum::Expected>> {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let mut digests = Vec::new();
    if let Some(md5) = headers.get("content-md5") {
        let expected = md5
            .to_str()
            .ok()
            .and_then(|v| checksum::Expected::from_base64("content-md5", Algorithm::Md5, v))
            .ok_or_else(|| {
                S3Error::InvalidDigest("The Content-MD5 you specified was invalid.".into())
            })?;
        digests.push(expected);
    }
    for name in headers.keys() {
        let Some(algorithm) = name.as_str().strip_prefix("x-amz-checksum-") else {
            continue;
        };
        if algorithm == "type" {
            if !header_is(headers, name.as_str(), "FULL_OBJECT") {
                return Err(S3Error::NotImplemented(
                    "only FULL_OBJECT checksums are implemented".into(),
                ));
            }
            continue;
        }
        let Some(algorithm) = Algorithm::from_checksum_name(algorithm) else {
            return Err(S3Error::NotImplemented(format!(
                "the {name} checksum is not implemented"
            )));
        };
        let expected = value(name.as_str())
            .and_then(|v| checksum::Expected::from_base64(name.as_str(), algorithm, v))
            .ok_or_else(|| {
                S3Error::InvalidArgument(format!("Value for {name} header is invalid."))
            })?;
        digests.push(expected);
    }
    if let Some(algorithm) = value("x-amz-sdk-checksum-algorithm") {
        if Algorithm::from_checksum_name(algorithm).is_none() {
            return Err(S3Error::NotImplemented(format!(
                "the {algorithm} checksum is not implemented"
            )));
        }
    }
    // SigV4's payload hash. UNSIGNED-PAYLOAD says there's none, and in the
    // streaming forms each chunk is signed instead (#178).
    if let Some(sha) = value("x-amz-content-sha256") {
        if !sha.eq_ignore_ascii_case("UNSIGNED-PAYLOAD") && !is_streaming(sha) {
            let expected =
                checksum::Expected::from_hex("x-amz-content-sha256", Algorithm::Sha256, sha)
                    .ok_or_else(|| {
                        S3Error::InvalidArgument(
                            "x-amz-content-sha256 must be UNSIGNED-PAYLOAD or the body's \
                             SHA-256 in hex"
                                .into(),
                        )
                    })?;
            digests.push(expected);
        }
    }
    Ok(digests)
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
        let mut values = headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| S3Error::InvalidArgument(format!("the {name} header must be ASCII")))?
            .join(",");
        // aws-chunked is how the upload was framed, not how the object is
        // encoded, so it isn't kept (#178).
        if name == header::CONTENT_ENCODING.as_str() {
            values = values
                .split(',')
                .filter(|token| !is_aws_chunked(token))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(",");
            if values.is_empty() {
                continue;
            }
        }
        metadata.push((name.to_string(), values));
    }
    metadata.sort();
    Ok(metadata)
}

/// Requests no route serves, such as multipart uploads, get an S3 error a
/// client can read instead of an empty 404 or 405 (#156).
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

    let parsed = stored_metadata(&request_headers).and_then(|metadata| {
        let digests = expected_digests(&request_headers)?;
        Ok((metadata, digests, chunked_body(&request_headers)?))
    });
    let options = match parsed {
        Ok((metadata, digests, chunked)) => PutOptions {
            conditions: preconditions(&request_headers),
            metadata,
            digests,
            chunked,
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

/// The ListBuckets query parameters `list_buckets` reads.
const LIST_BUCKETS_PARAMS: &[&str] = &["prefix", "max-buckets", "continuation-token"];

/// S3 caps a ListBuckets page at 10000 buckets.
const MAX_BUCKETS: usize = 10_000;

/// GET /: ListBuckets, as `ListAllMyBucketsResult` XML (#159). Buckets come
/// back in byte order, at most `max-buckets` of them a page.
pub async fn list_buckets(State(s3): State<AppState>, Query(query): RawQuery) -> Response {
    if let Err(e) = check_query(&query, LIST_BUCKETS_PARAMS, &["ListBuckets"]) {
        return e.into_response();
    }
    let param = |name: &str| {
        query
            .iter()
            .rev()
            .find(|(p, _)| p == name)
            .map(|(_, v)| v.as_str())
    };
    let prefix = param("prefix").unwrap_or("");
    let max = match param("max-buckets").map(str::parse::<usize>) {
        None => MAX_BUCKETS,
        Some(Ok(n)) if (1..=MAX_BUCKETS).contains(&n) => n,
        Some(_) => {
            return S3Error::InvalidArgument(format!(
                "max-buckets must be between 1 and {MAX_BUCKETS}"
            ))
            .into_response()
        }
    };
    let after = match param("continuation-token") {
        None => None,
        Some(token) => match decode_token(token) {
            Some(name) => Some(name),
            None => {
                return S3Error::InvalidArgument("The continuation token is not valid".into())
                    .into_response()
            }
        },
    };
    let mut buckets = match s3.list_buckets(prefix, after.as_deref(), max + 1) {
        Ok(buckets) => buckets,
        Err(e) => return e.into_response(),
    };
    let next = (buckets.len() > max).then(|| {
        buckets.truncate(max);
        encode_token(&buckets[max - 1].0)
    });

    let mut x = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListAllMyBucketsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Buckets>"#,
    );
    for (name, created_at) in &buckets {
        x.push_str("<Bucket>");
        element(&mut x, "Name", name);
        element(&mut x, "CreationDate", &iso8601(*created_at));
        x.push_str("</Bucket>");
    }
    x.push_str("</Buckets>");
    if let Some(token) = next {
        element(&mut x, "ContinuationToken", &token);
    }
    if !prefix.is_empty() {
        element(&mut x, "Prefix", prefix);
    }
    x.push_str("</ListAllMyBucketsResult>");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        x,
    )
        .into_response()
}

/// DELETE /{bucket}: DeleteBucket (#159). As in S3, the bucket must hold no
/// objects.
pub async fn delete_bucket(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): RawQuery,
) -> Response {
    info!("DELETE BUCKET /{}", bucket);
    // `DELETE /{bucket}?policy` and the like delete a bucket's
    // configuration; they mustn't delete the bucket.
    if let Err(e) = check_query(&query, &[], &["DeleteBucket"]) {
        return e.into_response();
    }
    match s3.delete_bucket(&bucket).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

/// The largest DeleteObjects request body read.
const MAX_DELETE_BODY: usize = 8 * 1024 * 1024;

/// POST /{bucket}?delete: DeleteObjects (#159). The keys are deleted in one
/// journal batch. As with DELETE, a key that isn't there counts as deleted.
pub async fn delete_objects(
    State(s3): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): RawQuery,
    body: Body,
) -> Response {
    info!("DELETE OBJECTS /{}", bucket);
    // A POST to a bucket without `?delete` is a browser form upload.
    if !query.iter().any(|(name, _)| name == "delete") {
        return S3Error::NotImplemented(format!(
            "POST /{bucket} is implemented only as DeleteObjects (?delete)"
        ))
        .into_response();
    }
    if let Err(e) = check_query(&query, &["delete"], &["DeleteObjects"]) {
        return e.into_response();
    }
    let request = match axum::body::to_bytes(body, MAX_DELETE_BODY).await {
        Ok(bytes) => parse_delete(&bytes),
        Err(_) => Err(S3Error::MalformedXML(format!(
            "the Delete document can be at most {MAX_DELETE_BODY} bytes"
        ))),
    };
    let request = match request {
        Ok(request) => request,
        Err(e) => return e.into_response(),
    };
    // Without versioning, `null` is the only version there is.
    let versioned = |version: &Option<String>| version.as_deref().is_some_and(|v| v != "null");
    let keys: Vec<String> = request
        .objects
        .iter()
        .filter(|(_, version)| !versioned(version))
        .map(|(key, _)| key.clone())
        .collect();
    if let Err(e) = s3.delete_objects(&bucket, &keys).await {
        return e.into_response();
    }

    let mut x = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><DeleteResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
    );
    for (key, version) in &request.objects {
        if versioned(version) {
            x.push_str("<Error>");
            element(&mut x, "Key", key);
            element(&mut x, "VersionId", version.as_deref().unwrap_or_default());
            element(&mut x, "Code", "NotImplemented");
            element(&mut x, "Message", "Object versions are not implemented");
            x.push_str("</Error>");
        } else if !request.quiet {
            x.push_str("<Deleted>");
            element(&mut x, "Key", key);
            x.push_str("</Deleted>");
        }
    }
    x.push_str("</DeleteResult>");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        x,
    )
        .into_response()
}

/// S3 takes at most 1000 keys in one DeleteObjects request.
const MAX_DELETE_KEYS: usize = 1000;

/// A DeleteObjects request: each key, with its version id if it has one,
/// and whether only errors are reported.
#[derive(Debug, PartialEq)]
struct DeleteRequest {
    objects: Vec<(String, Option<String>)>,
    quiet: bool,
}

/// One piece of an XML document, as [`xml_events`] reads it.
#[derive(Debug)]
enum XmlEvent<'a> {
    Open(&'a str),
    Close(&'a str),
    Text(String),
}

/// Read a `Delete` document. Only its elements are read: `Object` holding
/// `Key` and an optional `VersionId` (and `ETag`, `LastModifiedTime` and
/// `Size`, which are ignored), and `Quiet`. Anything else is `MalformedXML`.
fn parse_delete(body: &[u8]) -> S3Result<DeleteRequest> {
    let bad = |why: &str| S3Error::MalformedXML(why.to_string());
    let doc = std::str::from_utf8(body).map_err(|_| bad("the document isn't UTF-8"))?;
    let leaf = |name: &str| {
        matches!(
            name,
            "Key" | "VersionId" | "ETag" | "LastModifiedTime" | "Size" | "Quiet"
        )
    };
    let mut stack: Vec<&str> = Vec::new();
    let mut text = String::new();
    let (mut key, mut version) = (None, None);
    let mut request = DeleteRequest {
        objects: Vec::new(),
        quiet: false,
    };
    let mut seen_root = false;
    for event in xml_events(doc.trim_start_matches('\u{feff}'))? {
        match event {
            XmlEvent::Open(name) => {
                let allowed = match stack.as_slice() {
                    [] => name == "Delete" && !seen_root,
                    ["Delete"] => matches!(name, "Object" | "Quiet"),
                    ["Delete", "Object"] => leaf(name) && name != "Quiet",
                    _ => false,
                };
                if !allowed {
                    return Err(bad(&format!("unexpected element <{name}>")));
                }
                seen_root = true;
                if name == "Object" {
                    (key, version) = (None, None);
                }
                stack.push(name);
                text.clear();
            }
            XmlEvent::Text(t) => {
                if stack.last().is_some_and(|name| leaf(name)) {
                    text.push_str(&t);
                } else if !t.trim().is_empty() {
                    return Err(bad("text outside an element that holds text"));
                }
            }
            XmlEvent::Close(name) => {
                if stack.pop() != Some(name) {
                    return Err(bad(&format!("unmatched </{name}>")));
                }
                match name {
                    "Key" => key = Some(std::mem::take(&mut text)),
                    "VersionId" => version = Some(std::mem::take(&mut text)),
                    "Quiet" => {
                        request.quiet = match text.trim() {
                            "true" => true,
                            "false" => false,
                            _ => return Err(bad("Quiet must be true or false")),
                        }
                    }
                    "Object" => {
                        let key = key.take().ok_or_else(|| bad("an Object without a Key"))?;
                        request.objects.push((key, version.take()));
                    }
                    _ => {}
                }
                text.clear();
            }
        }
    }
    if !seen_root || !stack.is_empty() {
        return Err(bad("the document isn't one whole <Delete> element"));
    }
    if request.objects.is_empty() || request.objects.len() > MAX_DELETE_KEYS {
        return Err(bad(&format!(
            "a Delete holds 1 to {MAX_DELETE_KEYS} Objects"
        )));
    }
    Ok(request)
}

/// Split an XML document into elements and text, decoding entities and
/// character references. An XML declaration at the start is skipped.
/// Comments, CDATA, DOCTYPE and processing instructions are refused, so
/// nothing is read in a way an XML parser wouldn't.
fn xml_events(doc: &str) -> S3Result<Vec<XmlEvent<'_>>> {
    let bad = |why: &str| S3Error::MalformedXML(why.to_string());
    let mut rest = doc;
    if rest.starts_with("<?xml") {
        let end = rest
            .find("?>")
            .ok_or_else(|| bad("an unclosed XML declaration"))?;
        rest = &rest[end + 2..];
    }
    let mut events = Vec::new();
    while !rest.is_empty() {
        let Some(tag) = rest.strip_prefix('<') else {
            let end = rest.find('<').unwrap_or(rest.len());
            events.push(XmlEvent::Text(xml_unescape(&rest[..end])?));
            rest = &rest[end..];
            continue;
        };
        let end = tag.find('>').ok_or_else(|| bad("an unclosed tag"))?;
        let (inner, after) = (&tag[..end], &tag[end + 1..]);
        rest = after;
        if inner.starts_with(['!', '?']) {
            return Err(bad(
                "comments, CDATA, DOCTYPE and processing instructions aren't accepted",
            ));
        }
        if let Some(name) = inner.strip_prefix('/') {
            events.push(XmlEvent::Close(name.trim_end()));
            continue;
        }
        let (inner, empty) = match inner.strip_suffix('/') {
            Some(inner) => (inner, true),
            None => (inner, false),
        };
        let name = inner
            .split(|c: char| c.is_ascii_whitespace())
            .next()
            .unwrap_or("");
        if name.is_empty() {
            return Err(bad("a tag without a name"));
        }
        events.push(XmlEvent::Open(name));
        if empty {
            events.push(XmlEvent::Close(name));
        }
    }
    Ok(events)
}

/// Decode XML text: line ends become `\n`, as XML says, and the five
/// entities and character references become the characters they name.
fn xml_unescape(raw: &str) -> S3Result<String> {
    let bad = |why: &str| S3Error::MalformedXML(why.to_string());
    let raw = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let end = rest[i..]
            .find(';')
            .map(|j| i + j)
            .ok_or_else(|| bad("an unterminated entity"))?;
        let entity = &rest[i + 1..end];
        let c = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match entity.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => entity.strip_prefix('#').and_then(|d| d.parse().ok()),
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| bad(&format!("an unknown entity &{entity};")))?
            }
        };
        out.push(c);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    let xml_illegal = |c: char| matches!(c, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}');
    if out.chars().any(xml_illegal) {
        return Err(bad("a character XML can't carry"));
    }
    Ok(out)
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
    fn delete_documents_parse_as_s3_clients_send_them() {
        let doc = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\n",
            "  <Object><Key>a&amp;b&lt;c&gt;&quot;&apos;</Key></Object>\n",
            "  <Object><Key>cr&#13;lf&#x41;</Key><VersionId>null</VersionId></Object>\n",
            "  <Object><Key>  </Key><ETag>\"x\"</ETag></Object>\n",
            "  <Object><Key>line\r\nend</Key><VersionId>v1</VersionId></Object>\n",
            "  <Quiet>true</Quiet>\n",
            "</Delete>\n",
        );
        assert_eq!(
            parse_delete(doc.as_bytes()).unwrap(),
            DeleteRequest {
                objects: vec![
                    ("a&b<c>\"'".to_string(), None),
                    ("cr\rlfA".to_string(), Some("null".to_string())),
                    ("  ".to_string(), None),
                    ("line\nend".to_string(), Some("v1".to_string())),
                ],
                quiet: true,
            }
        );
        let plain = "<Delete><Object><Key>k</Key></Object></Delete>";
        assert!(!parse_delete(plain.as_bytes()).unwrap().quiet);
    }

    #[test]
    fn delete_documents_a_parser_could_read_differently_are_refused() {
        let object = "<Object><Key>k</Key></Object>";
        let too_many = format!("<Delete>{}</Delete>", object.repeat(MAX_DELETE_KEYS + 1));
        let bad = [
            "",
            "<Delete/>",
            "<Delete></Delete>",
            "<Delete><!-- k --><Object><Key>k</Key></Object></Delete>",
            "<Delete><Object><Key><![CDATA[k]]></Key></Object></Delete>",
            "<!DOCTYPE d><Delete><Object><Key>k</Key></Object></Delete>",
            "<Delete><Object><Key>k</Object></Key></Delete>",
            "<Delete><Object><Key>k</Key></Object>",
            "<Delete><Object><Name>k</Name></Object></Delete>",
            "<Delete><Object><Key>k</Key></Object></Delete><Delete/>",
            "<Delete>loose<Object><Key>k</Key></Object></Delete>",
            "<Delete><Object></Object></Delete>",
            "<Delete><Object><Key>k&nbsp;</Key></Object></Delete>",
            "<Delete><Object><Key>k&#1;</Key></Object></Delete>",
            "<Delete><Object><Key>k\u{1}</Key></Object></Delete>",
            "<Delete><Object><Key>k&amp</Key></Object></Delete>",
            "<Delete><Quiet>yes</Quiet><Object><Key>k</Key></Object></Delete>",
            "<s3:Delete><Object><Key>k</Key></Object></s3:Delete>",
            &too_many,
        ];
        for doc in bad {
            assert!(
                matches!(parse_delete(doc.as_bytes()), Err(S3Error::MalformedXML(_))),
                "{doc}"
            );
        }
        assert!(parse_delete(b"<Delete><Object><Key>\xff</Key></Object></Delete>").is_err());
        // Comments and the like are refused as such, not as odd elements.
        for doc in [
            "<Delete><!-- k --><Object><Key>k</Key></Object></Delete>",
            "<Delete><Object><Key><![CDATA[k]]></Key></Object></Delete>",
            "<!DOCTYPE d><Delete><Object><Key>k</Key></Object></Delete>",
            "<Delete><?pi x?><Object><Key>k</Key></Object></Delete>",
        ] {
            let refused = parse_delete(doc.as_bytes());
            assert!(
                matches!(&refused, Err(S3Error::MalformedXML(m)) if m.contains("comments, CDATA")),
                "{doc}: {refused:?}"
            );
        }
    }

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

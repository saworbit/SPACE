use anyhow::Error;
use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use futures::TryStreamExt;
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tracing::{error, info};

use crate::S3View;

pub type AppState = Arc<S3View>;

/// S3 Object metadata response
#[derive(Debug, Serialize)]
pub struct ObjectMetadata {
    pub key: String,
    pub size: u64,
    pub last_modified: u64,
    pub content_type: String,
    pub etag: String, // We'll use capsule_id as ETag
}

/// S3 List response
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub name: String,
    pub prefix: Option<String>,
    pub contents: Vec<ObjectMetadata>,
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
            (
                StatusCode::OK,
                [("ETag", format!("\"{}\"", capsule_id.as_uuid()))],
            )
                .into_response()
        }
        Err(e) => {
            error!("PUT failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// GET /{bucket}/{key}
pub async fn get_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    info!("GET /{}/{}", bucket, key);

    match s3.get_object(&bucket, &key).await {
        Ok(stream) => {
            info!("Streaming response for {}/{}", bucket, key);

            // Get metadata for Content-Type
            let content_type = s3
                .head_object(&bucket, &key)
                .map(|m| m.content_type)
                .unwrap_or_else(|_| "application/octet-stream".to_string());

            let body = Body::from_stream(stream);

            (StatusCode::OK, [("Content-Type", content_type)], body).into_response()
        }
        Err(e) => {
            error!("GET failed: {}", e);
            (StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
    }
}

/// HEAD /{bucket}/{key}
pub async fn head_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    info!("HEAD /{}/{}", bucket, key);

    match s3.head_object(&bucket, &key) {
        Ok(mapping) => {
            info!("HEAD {}/{} - {} bytes", bucket, key, mapping.size);
            let last_modified =
                httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(mapping.created_at));
            (
                StatusCode::OK,
                [
                    ("Content-Length", mapping.size.to_string()),
                    ("Content-Type", mapping.content_type.clone()),
                    ("ETag", format!("\"{}\"", mapping.capsule_id.as_uuid())),
                    ("Last-Modified", last_modified),
                ],
            )
                .into_response()
        }
        Err(e) => {
            error!("HEAD failed: {}", e);
            (StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
    }
}

/// GET /{bucket}?list
pub async fn list_objects(State(s3): State<AppState>, Path(bucket): Path<String>) -> Response {
    info!("LIST /{}", bucket);

    match s3.list_objects(&bucket) {
        Ok(mappings) => {
            let contents: Vec<ObjectMetadata> = mappings
                .iter()
                .map(|m| ObjectMetadata {
                    key: m.key.clone(),
                    size: m.size,
                    last_modified: m.created_at,
                    content_type: m.content_type.clone(),
                    etag: format!("\"{}\"", m.capsule_id.as_uuid()),
                })
                .collect();

            info!("Listed {} objects in {}", contents.len(), bucket);

            Json(ListResponse {
                name: bucket,
                prefix: None,
                contents,
            })
            .into_response()
        }
        Err(e) => {
            error!("LIST failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// DELETE /{bucket}/{key}
pub async fn delete_object(
    State(s3): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    info!("DELETE /{}/{}", bucket, key);

    match s3.delete_object(&bucket, &key) {
        Ok(_) => {
            info!("Deleted {}/{}", bucket, key);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("DELETE failed: {}", e);
            (StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
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

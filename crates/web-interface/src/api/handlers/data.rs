use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        HeaderValue, StatusCode,
    },
    response::{Json, Response},
    routing::get,
    Extension, Router,
};
use tracing::{debug, error, warn};
use validator::Validate;

use crate::api::{
    auth,
    errors::{ApiError, ApiResult},
    handlers::with_trace,
    models::{
        ApiResponse, ApiResponseFileUploadSchema, ApiResponseFilesListSchema, Claims, FileListItem,
        FileUploadResponse, FilesListResponse, Meta, PaginationQuery, RequestContext, UserRole,
    },
};
use crate::state::{AppState, MeshCommand, StoredFile};
use mesh_core::GossipMessage;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/data/objects", get(list_objects).post(upload_object))
        .route("/data/objects/*key", get(download_object).head(head_object))
}

/// List stored objects with pagination.
#[utoipa::path(
    get,
    path = "/api/v1/data/objects",
    tag = "Data",
    security(("jwt" = [])),
    params(PaginationQuery),
    responses((status = 200, body = ApiResponseFilesListSchema))
)]
pub async fn list_objects(
    State(state): State<AppState>,
    Query(pagination): Query<PaginationQuery>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<FilesListResponse> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;
    pagination
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let mut files: Vec<FileListItem> = state
        .files
        .read()
        .await
        .values()
        .map(|f| FileListItem {
            path: f.path.clone(),
            size: f.size,
            hash: f.hash.clone(),
            uploaded_at: f.uploaded_at,
        })
        .collect();

    files.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at));

    let total = files.len();
    let total_size = files.iter().map(|f| f.size).sum();
    let page = pagination.page.unwrap_or(1);
    let limit = pagination.limit.unwrap_or(50);
    let start = (page.saturating_sub(1) as usize).saturating_mul(limit as usize);
    let files = files.into_iter().skip(start).take(limit as usize).collect();

    let meta = with_trace(
        Meta::default().with_pagination(page, limit, total as u64, pagination.sort.clone()),
        Some(&ctx),
    );

    Ok(Json(ApiResponse::success(
        FilesListResponse {
            files,
            total,
            total_size,
        },
        Some(meta),
    )))
}

/// Upload an object via streaming multipart.
#[utoipa::path(
    post,
    path = "/api/v1/data/objects",
    tag = "Data",
    request_body(content = crate::api::models::UploadRequest, description = "Multipart with fields 'file' and optional 'path'", content_type = "multipart/form-data"),
    security(("jwt" = [])),
    responses(
        (status = 200, description = "Upload success", body = ApiResponseFileUploadSchema),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Payload too large")
    )
)]
pub async fn upload_object(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
    mut multipart: Multipart,
) -> ApiResult<FileUploadResponse> {
    auth::assert_role(&claims, &[UserRole::Admin, UserRole::Editor])?;
    debug!("POST /api/v1/data/objects");

    let mut path_field: Option<String> = None;
    let mut file_bytes: Vec<u8> = Vec::new();
    let mut hasher = blake3::Hasher::new();
    let mut size: u64 = 0;
    let mut file_name: Option<String> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart payload: {e}")))?
    {
        match field.name() {
            Some("path") => {
                let value = field
                    .text()
                    .await
                    .map_err(|e| ApiError::bad_request(format!("invalid path field: {e}")))?;
                if !value.trim().is_empty() {
                    path_field = Some(value);
                }
            }
            Some("file") => {
                if file_name.is_none() {
                    file_name = field.file_name().map(|f| f.to_string());
                }
                while let Some(chunk) = field.chunk().await.map_err(|e| {
                    ApiError::bad_request(format!("failed to read upload chunk: {e}"))
                })? {
                    size += chunk.len() as u64;
                    hasher.update(&chunk);
                    file_bytes.extend_from_slice(&chunk);
                }
            }
            _ => {}
        }
    }

    if file_bytes.is_empty() {
        return Err(ApiError::bad_request("missing file in multipart payload"));
    }

    let path = path_field
        .or(file_name)
        .ok_or_else(|| ApiError::bad_request("missing path or filename"))?;
    let normalized_path = normalize_path(&path);
    let hash = hasher.finalize().to_hex().to_string();
    let uploaded_at = current_unix_time();

    let stored_file = StoredFile {
        path: normalized_path.clone(),
        content: file_bytes.clone(),
        hash: hash.clone(),
        size,
        uploaded_at,
    };

    state
        .mesh_tx
        .send(MeshCommand::StoreFile { file: stored_file })
        .map_err(|err| {
            error!("failed to store file: {err}");
            ApiError::internal("failed to store object")
        })?;

    // Broadcast upload notification.
    let msg = GossipMessage::FileUploaded {
        path: normalized_path.clone(),
        size,
        uploader: claims.sub.clone(),
        hash: hash.clone(),
    };

    if let Err(err) = state.mesh_tx.send(MeshCommand::BroadcastGossip {
        topic: "data_ops".to_string(),
        msg,
    }) {
        warn!("failed to broadcast upload notification: {err}");
    }

    Ok(Json(ApiResponse::success(
        FileUploadResponse {
            path: normalized_path,
            hash,
            size,
            uploader: claims.sub,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Download an object as a binary stream.
#[utoipa::path(
    get,
    path = "/api/v1/data/objects/{key}",
    tag = "Data",
    security(("jwt" = [])),
    params(("key" = String, Path, description = "Object path or key")),
    responses((status = 200, description = "Binary stream"), (status = 404, description = "Not found"))
)]
pub async fn download_object(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Extension(claims): Extension<Claims>,
) -> Result<Response, ApiError> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let path = normalize_path(&key);
    let files = state.files.read().await;
    let file = files
        .get(&path)
        .ok_or_else(|| ApiError::not_found("object not found"))?;

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )
        .header(CONTENT_LENGTH, length_header(file.size))
        .body(Body::from(file.content.clone()))
        .map_err(|err| ApiError::internal(err.to_string()))
}

/// HEAD metadata endpoint for an object.
#[utoipa::path(
    head,
    path = "/api/v1/data/objects/{key}",
    tag = "Data",
    security(("jwt" = [])),
    params(("key" = String, Path, description = "Object path or key")),
    responses((status = 204, description = "Metadata returned in headers"), (status = 404, description = "Not found"))
)]
pub async fn head_object(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Extension(claims): Extension<Claims>,
) -> Result<Response, ApiError> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let path = normalize_path(&key);
    let files = state.files.read().await;
    let file = files
        .get(&path)
        .ok_or_else(|| ApiError::not_found("object not found"))?;

    let mut builder = Response::builder().status(StatusCode::NO_CONTENT);
    builder = builder
        .header(CONTENT_LENGTH, length_header(file.size))
        .header("ETag", etag_header(&file.hash));
    builder
        .body(Body::empty())
        .map_err(|err| ApiError::internal(err.to_string()))
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    }
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn length_header(size: u64) -> HeaderValue {
    HeaderValue::from_str(&size.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

fn etag_header(hash: &str) -> HeaderValue {
    HeaderValue::from_str(hash).unwrap_or_else(|_| HeaderValue::from_static("unknown"))
}

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{Json, Response},
    routing::get,
    Extension, Router,
};

use crate::api::{
    auth,
    errors::{ApiError, ApiResult},
    handlers::with_trace,
    models::{ApiResponse, Claims, HealthStatus, Meta, RequestContext, SystemInfo, UserRole},
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/system/health", get(health))
        .route("/system/info", get(info))
        .route("/system/metrics", get(metrics))
}

#[utoipa::path(
    get,
    path = "/api/v1/system/health",
    tag = "System",
    responses((status = 200, description = "Node is healthy", body = crate::api::models::ApiResponseHealthStatusSchema))
)]
pub async fn health(
    State(state): State<AppState>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<HealthStatus> {
    let uptime_ms = state.start_time.elapsed().as_millis();
    Ok(Json(ApiResponse::success(
        HealthStatus {
            status: "ok".to_string(),
            uptime_ms,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/system/info",
    tag = "System",
    security(("jwt" = [])),
    responses(
        (status = 200, description = "System information", body = crate::api::models::ApiResponseSystemInfoSchema),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn info(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<SystemInfo> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let peer_count = state.peers.read().await.len();
    let uptime_ms = state.start_time.elapsed().as_millis();
    let features = resolved_features();

    Ok(Json(ApiResponse::success(
        SystemInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            node_id: state.node_id.clone(),
            uptime_ms,
            features,
            peer_count,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/system/metrics",
    tag = "System",
    security(("jwt" = [])),
    responses((status = 200, description = "Prometheus metrics output"))
)]
pub async fn metrics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Response, ApiError> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let content = state
        .get_metrics()
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Body::from(content))
        .map_err(|err| ApiError::internal(err.to_string()))
}

fn resolved_features() -> Vec<String> {
    if let Ok(features) = std::env::var("SPACE_FEATURES") {
        return features
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    vec![
        "gossip".to_string(),
        "storage".to_string(),
        "control-plane".to_string(),
    ]
}

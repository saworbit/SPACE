use axum::{
    extract::State,
    routing::{get, post},
    Extension, Json, Router,
};
use tracing::warn;

use crate::api::{
    auth,
    errors::{ApiError, ApiResult},
    handlers::with_trace,
    models::{
        ApiResponse, ApiResponsePublishSchema, ApiResponseStatsSchema,
        ApiResponseSubscriptionsSchema, Claims, Meta, PublishRequest, PublishResponse,
        RequestContext, StatsResponse, SubscriptionsResponse, UserRole,
    },
};
use crate::state::{AppState, MeshCommand};
use mesh_core::GossipMessage;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/gossip/publish", post(publish))
        .route("/gossip/subscriptions", get(subscriptions))
        .route("/gossip/stats", get(stats))
}

/// Publish a custom gossip event.
#[utoipa::path(
    post,
    path = "/api/v1/gossip/publish",
    tag = "Gossip",
    security(("jwt" = [])),
    request_body = PublishRequest,
    responses((status = 200, body = ApiResponsePublishSchema), (status = 401, description = "Unauthorized"))
)]
pub async fn publish(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
    axum::Json(request): axum::Json<PublishRequest>,
) -> ApiResult<PublishResponse> {
    auth::assert_role(&claims, &[UserRole::Admin, UserRole::Editor])?;

    let msg = GossipMessage::Custom {
        topic: request.topic.clone(),
        payload: request.payload.clone(),
    };

    state
        .mesh_tx
        .send(MeshCommand::BroadcastGossip {
            topic: request.topic.clone(),
            msg,
        })
        .map_err(|_| ApiError::internal("failed to publish gossip event"))?;

    Ok(Json(ApiResponse::success(
        PublishResponse {
            topic: request.topic,
            routed: true,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// View current topic subscriptions.
#[utoipa::path(
    get,
    path = "/api/v1/gossip/subscriptions",
    tag = "Gossip",
    security(("jwt" = [])),
    responses((status = 200, body = ApiResponseSubscriptionsSchema))
)]
pub async fn subscriptions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<SubscriptionsResponse> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let stats = state
        .gossip
        .get_stats()
        .await
        .unwrap_or_else(|_| Default::default());

    let subscriptions = (0..stats.active_topics)
        .map(|i| crate::api::models::SubscriptionInfo {
            topic: format!("topic-{i}"),
            receivers: stats.connected_peers,
        })
        .collect();

    Ok(Json(ApiResponse::success(
        SubscriptionsResponse { subscriptions },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Gossip statistics endpoint.
#[utoipa::path(
    get,
    path = "/api/v1/gossip/stats",
    tag = "Gossip",
    security(("jwt" = [])),
    responses((status = 200, body = ApiResponseStatsSchema))
)]
pub async fn stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<StatsResponse> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let stats = state.gossip.get_stats().await.unwrap_or_else(|err| {
        warn!("failed to gather gossip stats: {err}");
        Default::default()
    });

    let mut additional = std::collections::HashMap::new();
    additional.insert(
        "peer_count".to_string(),
        serde_json::json!(state.peers.read().await.len()),
    );

    Ok(Json(ApiResponse::success(
        StatsResponse {
            stats: stats.into(),
            additional,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

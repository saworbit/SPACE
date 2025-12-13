use std::{net::SocketAddr, time::Duration};

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Extension, Json, Router,
};
use tracing::{debug, error, warn};
use validator::Validate;

use crate::api::{
    auth,
    errors::{ApiError, ApiResult},
    handlers::with_trace,
    models::{
        ApiResponse, Claims, ConnectPeerRequest, MeshActionResponse, Meta, PaginationQuery,
        PeerView, PeersResponse, RequestContext, TopologyEdge, TopologyNode, TopologyResponse,
        UserRole,
    },
};
use crate::state::{AppState, MeshCommand};
use mesh_core::{NodeRole, Peer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/mesh/peers", get(list_peers))
        .route("/mesh/peers/:peer_id", get(get_peer).delete(remove_peer))
        .route("/mesh/topology", get(topology))
        .route("/mesh/connect", post(connect_peer))
}

/// List mesh peers with pagination.
#[utoipa::path(
    get,
    path = "/api/v1/mesh/peers",
    tag = "Mesh",
    security(("jwt" = [])),
    params(PaginationQuery),
    responses((status = 200, body = ApiResponse<PeersResponse>), (status = 401, description = "Unauthorized"))
)]
pub async fn list_peers(
    State(state): State<AppState>,
    Query(pagination): Query<PaginationQuery>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<PeersResponse> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;
    pagination
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    debug!("GET /api/v1/mesh/peers");
    if let Err(err) = state.mesh_tx.send(MeshCommand::RefreshPeers) {
        warn!("failed to trigger peer refresh: {err}");
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    let peers_guard = state.peers.read().await;
    let total_count = peers_guard.len();
    let page = pagination.page.unwrap_or(1);
    let limit = pagination.limit.unwrap_or(50);
    let start = (page.saturating_sub(1) as usize).saturating_mul(limit as usize);

    let peers: Vec<PeerView> = peers_guard
        .iter()
        .skip(start)
        .take(limit as usize)
        .cloned()
        .map(PeerView::from)
        .collect();

    let stats = state
        .gossip
        .get_stats()
        .await
        .unwrap_or_else(|_| Default::default());

    let mut gossip_metrics = std::collections::HashMap::new();
    gossip_metrics.insert("convergence_time_ms".to_string(), stats.avg_convergence_ms);
    gossip_metrics.insert("duplication_rate".to_string(), stats.duplication_rate);
    gossip_metrics.insert("bandwidth_usage".to_string(), stats.bandwidth_usage as f64);

    let meta = with_trace(
        Meta::default().with_pagination(page, limit, total_count as u64, pagination.sort.clone()),
        Some(&ctx),
    );

    Ok(Json(ApiResponse::success(
        PeersResponse {
            peers,
            gossip_metrics,
            total_count,
        },
        Some(meta),
    )))
}

/// Retrieve a specific peer by id.
#[utoipa::path(
    get,
    path = "/api/v1/mesh/peers/{peer_id}",
    tag = "Mesh",
    security(("jwt" = [])),
    params(("peer_id" = String, Path, description = "Peer identifier")),
    responses((status = 200, body = ApiResponse<PeerView>), (status = 404, description = "Peer not found"))
)]
pub async fn get_peer(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<PeerView> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let peers = state.peers.read().await;
    let peer = peers
        .iter()
        .find(|p| p.id == peer_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("peer not found"))?;

    Ok(Json(ApiResponse::success(
        PeerView::from(peer),
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Summarize mesh topology for visualization.
#[utoipa::path(
    get,
    path = "/api/v1/mesh/topology",
    tag = "Mesh",
    security(("jwt" = [])),
    responses((status = 200, body = ApiResponse<TopologyResponse>))
)]
pub async fn topology(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<TopologyResponse> {
    auth::assert_role(
        &claims,
        &[UserRole::Viewer, UserRole::Editor, UserRole::Admin],
    )?;

    let peers = state.peers.read().await;
    let nodes = peers
        .iter()
        .map(|p| TopologyNode {
            id: p.id.clone(),
            role: p.role.clone().into(),
            status: p.status.clone(),
        })
        .collect();

    let response = TopologyResponse {
        nodes,
        edges: Vec::<TopologyEdge>::new(),
    };

    Ok(Json(ApiResponse::success(
        response,
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Connect to a peer manually (admin only).
#[utoipa::path(
    post,
    path = "/api/v1/mesh/connect",
    tag = "Mesh",
    security(("jwt" = [])),
    request_body = ConnectPeerRequest,
    responses((status = 200, body = ApiResponse<MeshActionResponse>), (status = 401, description = "Unauthorized"))
)]
pub async fn connect_peer(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
    axum::Json(request): axum::Json<ConnectPeerRequest>,
) -> ApiResult<MeshActionResponse> {
    auth::assert_role(&claims, &[UserRole::Admin])?;
    request
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let addr: SocketAddr = request
        .address
        .parse()
        .map_err(|_| ApiError::bad_request("invalid peer socket address"))?;

    let peer = Peer::new(
        request.peer_id.clone(),
        addr,
        NodeRole::from(request.role.clone()),
    );
    if let Err(err) = state
        .mesh_tx
        .send(MeshCommand::AddPeer { peer: peer.clone() })
    {
        error!("failed to send add peer command: {err}");
        return Err(ApiError::internal("failed to enqueue peer connect"));
    }

    let message = format!("peer {} scheduled for connect", request.peer_id);
    Ok(Json(ApiResponse::success(
        MeshActionResponse {
            peer_id: peer.id,
            message,
            accepted: true,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Remove or ban a peer (admin only).
#[utoipa::path(
    delete,
    path = "/api/v1/mesh/peers/{peer_id}",
    tag = "Mesh",
    security(("jwt" = [])),
    params(("peer_id" = String, Path, description = "Peer identifier")),
    responses((status = 200, body = ApiResponse<MeshActionResponse>), (status = 404, description = "Peer not found"))
)]
pub async fn remove_peer(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    Extension(claims): Extension<Claims>,
    Extension(ctx): Extension<RequestContext>,
) -> ApiResult<MeshActionResponse> {
    auth::assert_role(&claims, &[UserRole::Admin])?;

    let exists = state.peers.read().await.iter().any(|p| p.id == peer_id);
    if !exists {
        return Err(ApiError::not_found("peer not found"));
    }

    state
        .mesh_tx
        .send(MeshCommand::RemovePeer {
            peer_id: peer_id.clone(),
        })
        .map_err(|err| {
            error!("failed to send remove peer command: {err}");
            ApiError::internal("failed to enqueue peer removal")
        })?;

    Ok(Json(ApiResponse::success(
        MeshActionResponse {
            peer_id,
            message: "peer removal scheduled".to_string(),
            accepted: true,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

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
    models::{
        ApiResponse, Claims, HealthCheck, HealthStatus, Meta, RequestContext, SystemInfo, UserRole,
    },
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
    let checks = run_health_checks(&state).await;

    let status = if checks.iter().any(|c| c.severity == "error") {
        "error"
    } else if checks.iter().any(|c| c.severity == "warn") {
        "warn"
    } else {
        "ok"
    };

    Ok(Json(ApiResponse::success(
        HealthStatus {
            status: status.to_string(),
            uptime_ms,
            checks,
        },
        Some(with_trace(Meta::default(), Some(&ctx))),
    )))
}

/// Run real subsystem health checks with severity-based status derivation.
async fn run_health_checks(state: &AppState) -> Vec<HealthCheck> {
    let mut checks = Vec::new();

    // ── Gossip connectivity ─────────────────────────────────────
    match state.gossip.get_peers().await {
        Ok(peers) => {
            if peers.is_empty() {
                checks.push(HealthCheck {
                    id: "GOSSIP_NO_PEERS".into(),
                    severity: "warn".into(),
                    message: "No gossip peers connected; node is operating in standalone mode"
                        .into(),
                });
            } else {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let online = peers.iter().filter(|p| p.is_online(now, 30)).count();
                if online < peers.len() {
                    checks.push(HealthCheck {
                        id: "GOSSIP_PEERS_DEGRADED".into(),
                        severity: "warn".into(),
                        message: format!("{}/{} gossip peers online", online, peers.len()),
                    });
                } else {
                    checks.push(HealthCheck {
                        id: "GOSSIP_OK".into(),
                        severity: "ok".into(),
                        message: format!("{} peers connected", peers.len()),
                    });
                }
            }
        }
        Err(_) => {
            checks.push(HealthCheck {
                id: "GOSSIP_UNREACHABLE".into(),
                severity: "error".into(),
                message: "Failed to query gossip layer".into(),
            });
        }
    }

    // ── Gossip stats ────────────────────────────────────────────
    if let Ok(stats) = state.gossip.get_stats().await {
        if stats.messages_sent == 0 && state.start_time.elapsed().as_secs() > 30 {
            checks.push(HealthCheck {
                id: "GOSSIP_NO_MESSAGES".into(),
                severity: "warn".into(),
                message: "No gossip messages sent since startup (>30s)".into(),
            });
        }
    }

    // ── Metrics subsystem ───────────────────────────────────────
    let families = state.metrics.gather();
    if families.is_empty() {
        checks.push(HealthCheck {
            id: "METRICS_EMPTY".into(),
            severity: "warn".into(),
            message: "Prometheus registry has no metrics registered".into(),
        });
    }

    checks
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── resolved_features ─────────────────────────────────────────

    #[test]
    fn resolved_features_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SPACE_FEATURES");

        let features = resolved_features();
        assert_eq!(features, vec!["gossip", "storage", "control-plane"]);
    }

    #[test]
    fn resolved_features_from_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_FEATURES", "alpha,beta,gamma");

        let features = resolved_features();
        assert_eq!(features, vec!["alpha", "beta", "gamma"]);
        std::env::remove_var("SPACE_FEATURES");
    }

    #[test]
    fn resolved_features_trims_whitespace() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_FEATURES", " a , b , c ");

        let features = resolved_features();
        assert_eq!(features, vec!["a", "b", "c"]);
        std::env::remove_var("SPACE_FEATURES");
    }

    #[test]
    fn resolved_features_skips_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_FEATURES", "a,,b,,");

        let features = resolved_features();
        assert_eq!(features, vec!["a", "b"]);
        std::env::remove_var("SPACE_FEATURES");
    }
}

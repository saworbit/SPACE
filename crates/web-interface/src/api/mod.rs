//! REST API composition for the SPACE Control Plane.
//!
//! This module wires the versioned routers, middleware, and OpenAPI surface.

pub mod auth;
pub mod docs;
pub mod errors;
pub mod handlers;
pub mod models;

use axum::{middleware, Router};

use crate::state::AppState;

/// Build the versioned API router.
pub fn router(state: AppState) -> Router<AppState> {
    let v1 = Router::new()
        .merge(handlers::system::routes())
        .merge(handlers::mesh::routes())
        .merge(handlers::data::routes())
        .merge(handlers::gossip::routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    Router::new().nest("/v1", v1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use mesh_core::{GossipHandler, GossipMessage, GossipStats, Peer, Result};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    // Env-var lock for JWT_SECRET
    static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

    async fn set_test_jwt_secret() -> tokio::sync::MutexGuard<'static, ()> {
        let lock = ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
        let guard = lock.lock().await;
        std::env::set_var("JWT_SECRET", "test-integration-secret");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");
        std::env::remove_var("SPACE_DEV_GOD_TOKEN");
        guard
    }

    fn make_jwt(role: &str) -> String {
        use jsonwebtoken::{encode, EncodingKey, Header};
        let claims = serde_json::json!({
            "sub": "test-user",
            "role": role,
            "exp": 9999999999u64,
        });
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"test-integration-secret"),
        )
        .unwrap()
    }

    fn auth_request(uri: &str, method: &str, role: &str) -> Request<Body> {
        let token = make_jwt(role);
        Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    fn auth_request_json(uri: &str, method: &str, role: &str, body: &str) -> Request<Body> {
        let token = make_jwt(role);
        Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    struct NoopGossip;

    #[async_trait::async_trait]
    impl GossipHandler for NoopGossip {
        async fn broadcast(&self, _topic: &str, _msg: GossipMessage) -> Result<()> {
            Ok(())
        }

        async fn subscribe(&self, _topic: &str) -> Result<mpsc::Receiver<GossipMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }

        async fn pull_state(&self, _peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
            Ok(HashMap::new())
        }

        async fn get_peers(&self) -> Result<Vec<Peer>> {
            Ok(Vec::new())
        }

        async fn get_stats(&self) -> Result<GossipStats> {
            Ok(GossipStats::default())
        }
    }

    #[tokio::test]
    async fn health_is_public() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn mesh_requires_auth() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/mesh/peers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── Health endpoint body validation ─────────────────────────────

    #[tokio::test]
    async fn health_response_contains_status_field() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["success"].as_bool().unwrap(), "success should be true");
        let status = json["data"]["status"].as_str().unwrap();
        assert!(
            ["ok", "warn", "error"].contains(&status),
            "status should be one of ok/warn/error, got: {status}"
        );
    }

    #[tokio::test]
    async fn health_response_includes_checks_array() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let checks = json["data"]["checks"].as_array();
        assert!(checks.is_some(), "checks should be an array");
        let checks = checks.unwrap();
        assert!(!checks.is_empty(), "checks should not be empty");

        // Each check should have id, severity, message
        for check in checks {
            assert!(check["id"].as_str().is_some(), "check should have an id");
            assert!(
                check["severity"].as_str().is_some(),
                "check should have a severity"
            );
            assert!(
                check["message"].as_str().is_some(),
                "check should have a message"
            );
        }
    }

    #[tokio::test]
    async fn health_response_includes_uptime() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let uptime = json["data"]["uptime_ms"].as_u64();
        assert!(uptime.is_some(), "uptime_ms should be present");
    }

    // ── Health with no gossip peers: should warn ────────────────────

    #[tokio::test]
    async fn health_no_peers_warns_gossip() {
        let state = AppState::new(Arc::new(NoopGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let checks = json["data"]["checks"].as_array().unwrap();
        let gossip_check = checks
            .iter()
            .find(|c| c["id"].as_str().unwrap().starts_with("GOSSIP_"));
        assert!(gossip_check.is_some(), "should have a GOSSIP_* check");

        let check = gossip_check.unwrap();
        assert_eq!(check["id"].as_str().unwrap(), "GOSSIP_NO_PEERS");
        assert_eq!(check["severity"].as_str().unwrap(), "warn");
    }

    // ── Health with peers: gossip reports ok ────────────────────────

    struct GossipWithPeers;

    #[async_trait::async_trait]
    impl GossipHandler for GossipWithPeers {
        async fn broadcast(&self, _topic: &str, _msg: GossipMessage) -> Result<()> {
            Ok(())
        }
        async fn subscribe(&self, _topic: &str) -> Result<mpsc::Receiver<GossipMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }
        async fn pull_state(&self, _peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
            Ok(HashMap::new())
        }
        async fn get_peers(&self) -> Result<Vec<Peer>> {
            use mesh_core::NodeRole;
            use std::net::SocketAddr;
            let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
            let mut peer = Peer::new("peer-1".to_string(), addr, NodeRole::StorageNode);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            peer.last_gossip_heartbeat = now;
            Ok(vec![peer])
        }
        async fn get_stats(&self) -> Result<GossipStats> {
            Ok(GossipStats {
                messages_sent: 10,
                messages_received: 5,
                ..GossipStats::default()
            })
        }
    }

    #[tokio::test]
    async fn health_with_peers_reports_gossip_ok() {
        let state = AppState::new(Arc::new(GossipWithPeers));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let checks = json["data"]["checks"].as_array().unwrap();
        let gossip_check = checks
            .iter()
            .find(|c| c["id"].as_str().unwrap() == "GOSSIP_OK");
        assert!(
            gossip_check.is_some(),
            "with online peers, should have GOSSIP_OK check, got: {:?}",
            checks
        );
        assert_eq!(gossip_check.unwrap()["severity"].as_str().unwrap(), "ok");
    }

    // ── Health with failing gossip: error severity ──────────────────

    struct FailingGossip;

    #[async_trait::async_trait]
    impl GossipHandler for FailingGossip {
        async fn broadcast(&self, _topic: &str, _msg: GossipMessage) -> Result<()> {
            Err(mesh_core::CoreError::GossipFailure("broadcast fail".into()))
        }
        async fn subscribe(&self, _topic: &str) -> Result<mpsc::Receiver<GossipMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }
        async fn pull_state(&self, _peer_id: &str) -> Result<HashMap<String, Vec<u8>>> {
            Ok(HashMap::new())
        }
        async fn get_peers(&self) -> Result<Vec<Peer>> {
            Err(mesh_core::CoreError::GossipFailure(
                "gossip unreachable".into(),
            ))
        }
        async fn get_stats(&self) -> Result<GossipStats> {
            Err(mesh_core::CoreError::GossipFailure("stats fail".into()))
        }
    }

    #[tokio::test]
    async fn health_gossip_failure_produces_error() {
        let state = AppState::new(Arc::new(FailingGossip));
        let app = Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let status = json["data"]["status"].as_str().unwrap();
        assert_eq!(
            status, "error",
            "when gossip fails, overall status should be error"
        );

        let checks = json["data"]["checks"].as_array().unwrap();
        let unreachable = checks
            .iter()
            .find(|c| c["id"].as_str().unwrap() == "GOSSIP_UNREACHABLE");
        assert!(
            unreachable.is_some(),
            "should have GOSSIP_UNREACHABLE check"
        );
        assert_eq!(unreachable.unwrap()["severity"].as_str().unwrap(), "error");
    }

    // ── Health model serialization ──────────────────────────────────

    #[test]
    fn health_check_model_serde() {
        use crate::api::models::{HealthCheck, HealthStatus};

        let status = HealthStatus {
            status: "ok".to_string(),
            uptime_ms: 12345,
            checks: vec![HealthCheck {
                id: "TEST_CHECK".to_string(),
                severity: "ok".to_string(),
                message: "Everything fine".to_string(),
            }],
        };

        let json = serde_json::to_string(&status).unwrap();
        let restored: HealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.status, "ok");
        assert_eq!(restored.checks.len(), 1);
        assert_eq!(restored.checks[0].id, "TEST_CHECK");
    }

    #[test]
    fn health_status_empty_checks_skipped_in_json() {
        use crate::api::models::HealthStatus;

        let status = HealthStatus {
            status: "ok".to_string(),
            uptime_ms: 0,
            checks: vec![],
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("checks"),
            "empty checks should be skipped in serialization"
        );
    }

    // ── Authenticated endpoint integration tests ─────────────────────

    fn make_app() -> axum::Router {
        let state = AppState::new(Arc::new(NoopGossip));
        Router::new()
            .nest("/api", router(state.clone()))
            .with_state(state)
    }

    // ── mesh/peers ──────────────────────────────────────────────────

    #[tokio::test]
    async fn mesh_peers_with_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/mesh/peers", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["success"].as_bool().unwrap());
        assert!(json["data"]["peers"].is_array());
        assert!(json["data"]["total_count"].is_number());
    }

    // ── mesh/peers/:id ──────────────────────────────────────────────

    #[tokio::test]
    async fn mesh_get_peer_not_found() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/mesh/peers/nonexistent", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    // ── mesh/topology ───────────────────────────────────────────────

    #[tokio::test]
    async fn mesh_topology_returns_graph() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/mesh/topology", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["nodes"].is_array());
        assert!(json["data"]["edges"].is_array());
    }

    // ── mesh/connect (admin only) ───────────────────────────────────

    #[tokio::test]
    async fn mesh_connect_peer_as_admin() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let body = serde_json::json!({
            "peer_id": "new-peer-1",
            "address": "10.0.0.5:9090",
            "role": "storage_node"
        });
        let req = auth_request_json("/api/v1/mesh/connect", "POST", "admin", &body.to_string());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["accepted"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn mesh_connect_forbidden_for_viewer() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let body = serde_json::json!({
            "peer_id": "x",
            "address": "10.0.0.1:8080",
            "role": "viewer"
        });
        let req = auth_request_json("/api/v1/mesh/connect", "POST", "viewer", &body.to_string());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    // ── mesh/peers/:id DELETE (admin only) ──────────────────────────

    #[tokio::test]
    async fn mesh_remove_peer_not_found() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/mesh/peers/ghost", "DELETE", "admin");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mesh_remove_forbidden_for_editor() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/mesh/peers/any", "DELETE", "editor");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    // ── gossip/stats ────────────────────────────────────────────────

    #[tokio::test]
    async fn gossip_stats_with_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/gossip/stats", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["stats"].is_object());
        assert!(json["data"]["additional"].is_object());
    }

    #[tokio::test]
    async fn gossip_stats_requires_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = Request::builder()
            .uri("/api/v1/gossip/stats")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── gossip/subscriptions ────────────────────────────────────────

    #[tokio::test]
    async fn gossip_subscriptions_with_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/gossip/subscriptions", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["subscriptions"].is_array());
    }

    // ── gossip/publish ──────────────────────────────────────────────

    #[tokio::test]
    async fn gossip_publish_as_editor() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let body = serde_json::json!({
            "topic": "test-topic",
            "payload": [1, 2, 3]
        });
        let req = auth_request_json(
            "/api/v1/gossip/publish",
            "POST",
            "editor",
            &body.to_string(),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["topic"].as_str(), Some("test-topic"));
        assert!(json["data"]["routed"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn gossip_publish_forbidden_for_viewer() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let body = serde_json::json!({
            "topic": "x",
            "payload": []
        });
        let req = auth_request_json(
            "/api/v1/gossip/publish",
            "POST",
            "viewer",
            &body.to_string(),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    // ── system/info ─────────────────────────────────────────────────

    #[tokio::test]
    async fn system_info_with_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/system/info", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["version"].is_string());
        assert!(json["data"]["node_id"].is_string());
        assert!(json["data"]["uptime_ms"].is_number());
        assert!(json["data"]["features"].is_array());
    }

    #[tokio::test]
    async fn system_info_requires_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = Request::builder()
            .uri("/api/v1/system/info")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── system/metrics ──────────────────────────────────────────────

    #[tokio::test]
    async fn system_metrics_with_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/system/metrics", "GET", "admin");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("space_"), "should contain prometheus metrics");
    }

    // ── data/objects listing ────────────────────────────────────────

    #[tokio::test]
    async fn data_objects_list_empty() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/data/objects", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["files"].is_array());
        assert_eq!(json["data"]["total"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn data_objects_requires_auth() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = Request::builder()
            .uri("/api/v1/data/objects")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── data/objects/:key download not found ────────────────────────

    #[tokio::test]
    async fn data_download_not_found() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/data/objects/missing/file.txt", "GET", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    // ── data/objects/:key HEAD not found ─────────────────────────────

    #[tokio::test]
    async fn data_head_not_found() {
        let _lock = set_test_jwt_secret().await;
        let app = make_app();
        let req = auth_request("/api/v1/data/objects/no/such/key", "HEAD", "viewer");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }
}

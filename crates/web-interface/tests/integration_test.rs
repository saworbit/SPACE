//! Integration tests for the versioned SPACE Control Plane API.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, NodeRole, Peer};
use serde_json::Value;
use tower::ServiceExt;
use web_interface::{build_router, AppState};

async fn test_app() -> Router {
    let local_peer = Peer::new(
        "web-test".to_string(),
        "127.0.0.1:0".parse().unwrap(),
        NodeRole::Gateway,
    );
    let raft_port = local_peer.addr.port();
    let gossip = Arc::new(
        GossipImpl::new(GossipConfig::default(), local_peer, raft_port)
            .await
            .expect("gossip init"),
    ) as Arc<dyn mesh_core::GossipHandler>;
    build_router(AppState::new(gossip))
}

#[tokio::test]
async fn health_is_public_and_versioned() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.get("success"), Some(&Value::Bool(true)));
    assert_eq!(
        json.get("data")
            .and_then(|d| d.get("status"))
            .and_then(Value::as_str),
        Some("ok")
    );
}

#[tokio::test]
async fn mesh_peers_requires_auth() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/mesh/peers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metrics_require_auth() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn data_objects_require_auth() {
    let app = test_app().await;

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/data/objects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::UNAUTHORIZED);

    let upload_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/data/objects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload_response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn gossip_publish_requires_auth() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gossip/publish")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

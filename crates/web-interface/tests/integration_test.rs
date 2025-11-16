//! Integration tests for the web interface.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use gossip_layer::GossipImpl;
use mesh_core::GossipConfig;
use std::sync::Arc;
use tower::ServiceExt;
use web_interface::{build_router, AppState};

async fn create_test_app_state() -> AppState {
    let config = GossipConfig::default();
    let gossip = Arc::new(GossipImpl::new(config).await.expect("Failed to create gossip"));
    AppState::new(gossip)
}

#[tokio::test]
async fn test_health_check() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_peers_endpoint() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/peers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_gossip_stats_endpoint() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/gossip/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_metrics_endpoint() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_nonexistent_peer() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/peers/nonexistent-peer-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_upload_file_endpoint() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let upload_request = serde_json::json!({
        "path": "/test/file.txt",
        "content": base64::engine::general_purpose::STANDARD.encode(b"Hello, World!")
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&upload_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_broadcast_message_endpoint() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let broadcast_request = serde_json::json!({
        "topic": "test-topic",
        "payload": vec![1u8, 2, 3, 4]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/gossip/broadcast")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&broadcast_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_files_endpoint_empty() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_upload_and_list_files() {
    use axum::body::to_bytes;

    let state = create_test_app_state().await;
    let app = build_router(state.clone());

    // Upload a file first
    let upload_request = serde_json::json!({
        "path": "/test/myfile.txt",
        "content": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"Test content")
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&upload_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Wait for file to be stored
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // List files
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["total"], 1);
    assert_eq!(json["files"][0]["path"], "/test/myfile.txt");
}

#[tokio::test]
async fn test_download_file() {
    use axum::body::to_bytes;

    let state = create_test_app_state().await;
    let app = build_router(state.clone());

    let test_content = b"Hello, SPACE!";

    // Upload a file first
    let upload_request = serde_json::json!({
        "path": "/data/download-test.txt",
        "content": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, test_content)
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&upload_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Wait for file to be stored
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Download the file
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/files/data/download-test.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], test_content);
}

#[tokio::test]
async fn test_download_nonexistent_file() {
    let state = create_test_app_state().await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/files/nonexistent/file.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

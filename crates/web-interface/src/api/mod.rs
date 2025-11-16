//! REST API routes for the web interface.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use base64::Engine;
use mesh_core::{GossipMessage, Peer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error};

use crate::state::{AppState, MeshCommand, StoredFile};

/// Response for the peers endpoint
#[derive(Serialize)]
pub struct PeersResponse {
    /// List of known peers
    pub peers: Vec<Peer>,
    /// Gossip metrics
    pub gossip_metrics: HashMap<String, f64>,
    /// Total peer count
    pub total_count: usize,
}

/// Request for file upload
#[derive(Deserialize)]
pub struct UploadRequest {
    /// File path
    pub path: String,
    /// File content (base64 encoded)
    pub content: String,
}

/// Response for file operations
#[derive(Serialize)]
pub struct FileResponse {
    /// Success status
    pub success: bool,
    /// Message
    pub message: String,
    /// Optional file hash
    pub hash: Option<String>,
}

/// File listing item
#[derive(Serialize)]
pub struct FileListItem {
    /// File path
    pub path: String,
    /// File size in bytes
    pub size: u64,
    /// Blake3 hash
    pub hash: String,
    /// Upload timestamp (Unix epoch)
    pub uploaded_at: u64,
}

/// Response for file listing
#[derive(Serialize)]
pub struct FilesListResponse {
    /// List of files
    pub files: Vec<FileListItem>,
    /// Total file count
    pub total: usize,
    /// Total size in bytes
    pub total_size: u64,
}

/// Response for gossip stats
#[derive(Serialize)]
pub struct StatsResponse {
    /// Gossip statistics
    pub stats: mesh_core::GossipStats,
    /// Additional metrics
    pub additional: HashMap<String, serde_json::Value>,
}

/// Build API routes
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/peers", get(get_peers))
        .route("/peers/:peer_id", get(get_peer))
        .route("/upload", post(upload_file))
        .route("/files", get(list_files))
        .route("/files/*path", get(download_file))
        .route("/gossip/stats", get(get_gossip_stats))
        .route("/gossip/broadcast", post(broadcast_message))
        .route("/metrics", get(get_metrics))
}

/// Get all known peers
async fn get_peers(
    State(state): State<AppState>,
) -> Result<Json<PeersResponse>, StatusCode> {
    debug!("GET /api/peers");

    // Refresh peers from gossip
    if let Err(e) = state.mesh_tx.send(MeshCommand::RefreshPeers) {
        error!("Failed to send refresh command: {}", e);
    }

    // Wait a bit for refresh
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let peers = state.peers.read().await.clone();

    // Get gossip stats for metrics
    let stats = match state.gossip.get_stats().await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to get gossip stats: {}", e);
            Default::default()
        }
    };

    let mut gossip_metrics = HashMap::new();
    gossip_metrics.insert("convergence_time_ms".to_string(), stats.avg_convergence_ms);
    gossip_metrics.insert("duplication_rate".to_string(), stats.duplication_rate);
    gossip_metrics
        .insert("bandwidth_usage".to_string(), stats.bandwidth_usage as f64);

    Ok(Json(PeersResponse {
        total_count: peers.len(),
        peers,
        gossip_metrics,
    }))
}

/// Get a specific peer by ID
async fn get_peer(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
) -> Result<Json<Peer>, StatusCode> {
    debug!("GET /api/peers/{}", peer_id);

    let peers = state.peers.read().await;
    let peer = peers
        .iter()
        .find(|p| p.id == peer_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(peer))
}

/// Upload a file
async fn upload_file(
    State(state): State<AppState>,
    Json(request): Json<UploadRequest>,
) -> Result<Json<FileResponse>, StatusCode> {
    debug!("POST /api/upload - path: {}", request.path);

    // Decode base64 content
    let content = match base64::engine::general_purpose::STANDARD.decode(&request.content) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to decode base64: {}", e);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    // Calculate hash
    let hash = {
        use blake3::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(&content);
        hasher.finalize().to_hex().to_string()
    };

    let size = content.len() as u64;
    let uploaded_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Store the file
    let stored_file = StoredFile {
        path: request.path.clone(),
        content: content.clone(),
        hash: hash.clone(),
        size,
        uploaded_at,
    };

    if let Err(e) = state.mesh_tx.send(MeshCommand::StoreFile {
        file: stored_file,
    }) {
        error!("Failed to store file: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Broadcast upload notification via gossip
    let msg = GossipMessage::FileUploaded {
        path: request.path.clone(),
        size,
        uploader: "web-interface".to_string(),
        hash: hash.clone(),
    };

    if let Err(e) = state
        .mesh_tx
        .send(MeshCommand::BroadcastGossip {
            topic: "data_ops".to_string(),
            msg,
        })
    {
        error!("Failed to broadcast upload notification: {}", e);
    }

    Ok(Json(FileResponse {
        success: true,
        message: format!("File uploaded successfully: {}", request.path),
        hash: Some(hash),
    }))
}

/// List all stored files
async fn list_files(
    State(state): State<AppState>,
) -> Result<Json<FilesListResponse>, StatusCode> {
    debug!("GET /api/files");

    let files_lock = state.files.read().await;
    let mut files: Vec<FileListItem> = files_lock
        .values()
        .map(|f| FileListItem {
            path: f.path.clone(),
            size: f.size,
            hash: f.hash.clone(),
            uploaded_at: f.uploaded_at,
        })
        .collect();

    // Sort by upload time (newest first)
    files.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at));

    let total = files.len();
    let total_size = files.iter().map(|f| f.size).sum();

    Ok(Json(FilesListResponse {
        files,
        total,
        total_size,
    }))
}

/// Download a file by path
async fn download_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Vec<u8>, StatusCode> {
    // The path from axum includes the leading slash
    let clean_path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    debug!("GET /api/files{}", clean_path);

    let files_lock = state.files.read().await;
    let file = files_lock
        .get(&clean_path)
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(file.content.clone())
}

/// Get gossip statistics
async fn get_gossip_stats(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, StatusCode> {
    debug!("GET /api/gossip/stats");

    let stats = state
        .gossip
        .get_stats()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut additional = HashMap::new();
    additional.insert(
        "peer_count".to_string(),
        serde_json::json!(state.peers.read().await.len()),
    );

    Ok(Json(StatsResponse { stats, additional }))
}

/// Broadcast a custom gossip message
#[derive(Deserialize)]
pub struct BroadcastRequest {
    /// Topic to broadcast to
    pub topic: String,
    /// Message payload
    pub payload: Vec<u8>,
}

async fn broadcast_message(
    State(state): State<AppState>,
    Json(request): Json<BroadcastRequest>,
) -> Result<Json<FileResponse>, StatusCode> {
    debug!("POST /api/gossip/broadcast - topic: {}", request.topic);

    let msg = GossipMessage::Custom {
        topic: request.topic.clone(),
        payload: request.payload,
    };

    state
        .mesh_tx
        .send(MeshCommand::BroadcastGossip {
            topic: request.topic,
            msg,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(FileResponse {
        success: true,
        message: "Message broadcasted successfully".to_string(),
        hash: None,
    }))
}

/// Get Prometheus metrics
async fn get_metrics(
    State(state): State<AppState>,
) -> Result<String, StatusCode> {
    state
        .get_metrics()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}


#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use mesh_core::{GossipStats, Result};
    use tower::ServiceExt;

    // Mock gossip handler for testing
    struct MockGossipHandler;

    #[async_trait::async_trait]
    impl mesh_core::GossipHandler for MockGossipHandler {
        async fn broadcast(&self, _topic: &str, _msg: mesh_core::GossipMessage) -> Result<()> {
            Ok(())
        }

        async fn subscribe(&self, _topic: &str) -> Result<tokio::sync::mpsc::Receiver<mesh_core::GossipMessage>> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn pull_state(&self, _peer_id: &str) -> Result<std::collections::HashMap<String, Vec<u8>>> {
            Ok(std::collections::HashMap::new())
        }

        async fn get_peers(&self) -> Result<Vec<Peer>> {
            Ok(vec![])
        }

        async fn get_stats(&self) -> Result<GossipStats> {
            Ok(GossipStats::default())
        }
    }

    #[tokio::test]
    async fn test_get_peers() {
        let state = AppState::new(std::sync::Arc::new(MockGossipHandler));
        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/peers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}

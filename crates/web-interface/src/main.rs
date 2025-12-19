//! Web server entry point for the mesh data system.
//!
//! This starts the Axum HTTP server with all API routes, WebSocket endpoints,
//! and integration with the gossip layer.

use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, NodeRole, Peer, PeerStore};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;
use web_interface::{build_router, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "web_interface=debug,gossip_layer=debug,tower_http=debug,axum=trace".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting SPACE Web Interface");

    // Load configuration from environment or defaults
    let gossip_config = GossipConfig {
        fanout: std::env::var("GOSSIP_FANOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8),
        heartbeat_interval_ms: std::env::var("GOSSIP_HEARTBEAT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000),
        message_ttl: std::env::var("GOSSIP_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10),
        max_message_size: 4096,
        enable_compression: true,
        enable_encryption: true,
        signing_key: get_signing_key(),
    };

    info!(
        "Gossip config: fanout={}, heartbeat={}ms, ttl={}",
        gossip_config.fanout, gossip_config.heartbeat_interval_ms, gossip_config.message_ttl
    );

    // Get bind address from environment or use default
    let bind_addr = std::env::var("BIND_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 3000)));

    // Shared peer store for UI + control plane routing decisions
    let peer_store = PeerStore::new();

    // Local peer descriptor (gossip address is the HTTP bind addr by default).
    let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
    let gossip_addr: SocketAddr = std::env::var("GOSSIP_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(bind_addr);

    let local_peer = Peer::new(node_id.clone(), gossip_addr, NodeRole::Gateway);

    // Initialize gossip layer
    let gossip_impl = match GossipImpl::with_peer_store(gossip_config.clone(), local_peer, peer_store.clone()).await {
        Ok(g) => {
            info!("Gossip layer initialized successfully");
            g
        }
        Err(e) => {
            error!("Failed to initialize gossip layer: {}", e);
            return Err(e.into());
        }
    };

    let gossip: Arc<dyn mesh_core::GossipHandler> = Arc::new(gossip_impl);

    // Create application state
    let app_state = AppState::new(gossip);

    // Build the router
    let app = build_router(app_state);

    info!("Starting server on {}", bind_addr);

    // Start the server
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Get the signing key from environment or generate a default one.
///
/// In production, this should be loaded from a secure configuration source.
fn get_signing_key() -> Vec<u8> {
    if let Ok(key_hex) = std::env::var("GOSSIP_SIGNING_KEY") {
        if let Ok(key) = hex::decode(&key_hex) {
            if key.len() == 32 {
                info!("Loaded signing key from environment");
                return key;
            }
        }
        error!("Invalid GOSSIP_SIGNING_KEY in environment, using default");
    }

    // Default key (NOT SECURE - only for development)
    let default_key = vec![0u8; 32];
    info!("Using default signing key (NOT SECURE - development only)");
    default_key
}

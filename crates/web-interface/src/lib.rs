//! Next-gen web interface for the mesh data system.
//!
//! This crate provides a comprehensive web interface for managing and
//! monitoring the mesh network with gossip protocol integration.
//!
//! # Features
//!
//! - Real-time mesh topology visualization
//! - Gossip protocol monitoring and metrics
//! - File upload/download with chunked transfers
//! - Data transformation operations
//! - WebSocket-based live updates
//! - Admin panel for configuration
//!
//! # Architecture
//!
//! The web interface consists of:
//! - Axum-based HTTP server with REST APIs
//! - WebSocket endpoints for real-time updates
//! - Leptos-based reactive frontend (optional, feature-gated)
//! - Integration with gossip-layer for mesh communication

pub mod api;
pub mod state;
pub mod ws;

#[cfg(feature = "frontend")]
pub mod frontend;

pub use state::AppState;

use axum::{extract::DefaultBodyLimit, response::Html, routing::get, Router};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// Build the main application router with all routes.
pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_check))
        .nest("/api", api::router())
        .nest("/ws", ws::routes())
        .merge(api::docs::swagger_routes())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        // Increase body size limit to 100MB for file uploads
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
}

/// Root handler - welcome page
async fn root_handler() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// Health check endpoint
async fn health_check() -> &'static str {
    "OK"
}

#[cfg(test)]
mod tests {
    // Tests are in individual modules (api, ws, state)
}

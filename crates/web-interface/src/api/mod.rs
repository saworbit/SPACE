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
pub fn router() -> Router<AppState> {
    let v1 = Router::new()
        .merge(handlers::system::routes())
        .merge(handlers::mesh::routes())
        .merge(handlers::data::routes())
        .merge(handlers::gossip::routes())
        .layer(middleware::from_fn(auth::auth_middleware));

    Router::new().nest("/v1", v1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_is_public() {
        let state = AppState::default();
        let app = Router::new().nest("/api", router()).with_state(state);

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
        let state = AppState::default();
        let app = Router::new().nest("/api", router()).with_state(state);

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
}

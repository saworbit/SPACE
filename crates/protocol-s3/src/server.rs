use anyhow::Result;
use axum::{
    routing::{delete, get, head, put},
    Router,
};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::{handlers::*, S3View};

pub struct S3Server {
    s3_view: Arc<S3View>,
    port: u16,
}

impl S3Server {
    pub fn new(s3_view: S3View, port: u16) -> Self {
        Self {
            s3_view: Arc::new(s3_view),
            port,
        }
    }

    pub async fn run(self) -> Result<()> {
        // Build router with S3-compatible endpoints
        let app = Router::new()
            // Health check
            .route("/health", get(health_check))
            
            // S3 Object Operations
            .route("/:bucket/:key", put(put_object))
            .route("/:bucket/:key", get(get_object))
            .route("/:bucket/:key", head(head_object))
            .route("/:bucket/:key", delete(delete_object))
            
            // Bucket Operations
            .route("/:bucket", get(list_objects))
            
            // Add state
            .with_state(self.s3_view)
            
            // Add middleware
            .layer(CorsLayer::permissive())
            .layer(TraceLayer::new_for_http());

        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        
        info!("🚀 SPACE S3 Protocol View listening on http://{}", addr);
        info!("📦 Ready to serve capsules via S3 API");
        info!("");
        info!("Try:");
        info!("  curl -X PUT http://localhost:{}/demo-bucket/hello.txt -d 'Hello SPACE!'", self.port);
        info!("  curl http://localhost:{}/demo-bucket/hello.txt", self.port);
        info!("");

        axum::serve(listener, app).await?;

        Ok(())
    }
}
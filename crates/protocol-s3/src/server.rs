use anyhow::Result;

use axum::{routing::get, Router};

use std::net::SocketAddr;
use std::sync::Arc;

use tower_http::trace::TraceLayer;
use tracing::info;

use crate::{handlers::*, S3View};

/// The S3 routes. Keys may contain '/', so the key is a wildcard (#88).
pub fn router(s3_view: Arc<S3View>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(health_check))
        // Bucket Operations
        .route(
            "/{bucket}",
            get(list_objects).put(create_bucket).head(head_bucket),
        )
        // S3 Object Operations
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .put(put_object)
                .head(head_object)
                .delete(delete_object),
        )
        .with_state(s3_view)
        .layer(TraceLayer::new_for_http())
}

/// The server has no authentication and sends no CORS headers, so browsers
/// on other origins can't call it. Bind it to a non-loopback address only on
/// a trusted network.
pub struct S3Server {
    s3_view: Arc<S3View>,
    addr: SocketAddr,
}

impl S3Server {
    pub fn new(s3_view: S3View, addr: SocketAddr) -> Self {
        Self {
            s3_view: Arc::new(s3_view),
            addr,
        }
    }

    pub async fn run(self) -> Result<()> {
        let app = router(self.s3_view);

        let addr = self.addr;
        let listener = tokio::net::TcpListener::bind(addr).await?;

        info!("🚀 SPACE S3 Protocol View listening on http://{}", addr);
        info!("📦 Ready to serve capsules via S3 API");
        info!("");
        info!("Try:");
        info!(
            "  curl -X PUT http://localhost:{}/demo-bucket/hello.txt -d 'Hello SPACE!'",
            addr.port()
        );
        info!(
            "  curl http://localhost:{}/demo-bucket/hello.txt",
            addr.port()
        );
        info!("");

        axum::serve(listener, app).await?;

        Ok(())
    }
}

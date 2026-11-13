use anyhow::Result;

use axum::{routing::get, Router};

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};

use crate::{handlers::*, S3View};
use common::scrub::ScrubConfig;

/// Run [`S3View::reclaim`] now and then every `every`, until the task is
/// aborted. A failed pass is logged and the next one runs on schedule.
pub fn spawn_reclaim(s3_view: Arc<S3View>, every: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            match s3_view.reclaim().await {
                Ok(report)
                    if report.collected > 0
                        || report.compacted.is_some()
                        || report.journal_compacted.is_some() =>
                {
                    info!(
                        collected = report.collected,
                        compacted_bytes = report.compacted,
                        journal_compacted_bytes = report.journal_compacted,
                        "reclaimed space"
                    )
                }
                Ok(_) => debug!("nothing worth reclaiming"),
                Err(err) => warn!("reclaim pass failed: {err:#}"),
            }
        }
    })
}

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
    reclaim_every: Option<Duration>,
    scrub: Option<ScrubConfig>,
}

impl S3Server {
    pub fn new(s3_view: S3View, addr: SocketAddr) -> Self {
        Self {
            s3_view: Arc::new(s3_view),
            addr,
            reclaim_every: None,
            scrub: None,
        }
    }

    /// Scrub the store in the background on `config`'s schedule while the
    /// server runs ([`S3View::spawn_scrub`]). `None`, the default, doesn't.
    pub fn scrub(mut self, config: Option<ScrubConfig>) -> Self {
        self.scrub = config;
        self
    }

    /// Run a reclaim pass at startup and then every `every` while the
    /// server runs ([`spawn_reclaim`]). `None`, the default, runs none.
    pub fn reclaim_every(mut self, every: Option<Duration>) -> Self {
        self.reclaim_every = every;
        self
    }

    pub async fn run(self) -> Result<()> {
        let _reclaim = self
            .reclaim_every
            .map(|every| AbortOnDrop(spawn_reclaim(Arc::clone(&self.s3_view), every)));
        let _scrub = match self.scrub {
            // Each cycle logs its outcome, so nothing reads the state.
            Some(config) => Some(AbortOnDrop(self.s3_view.spawn_scrub(config).await.0)),
            None => None,
        };
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

/// Stops the reclaim task when the server stops.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

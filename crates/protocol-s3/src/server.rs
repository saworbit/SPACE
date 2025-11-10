use anyhow::Result;
#[cfg(feature = "advanced-security")]
use axum::{
    body::Body,
    http::Request,
    middleware::{from_fn, Next},
    response::Response,
};
use axum::{
    routing::{delete, get, head, put},
    Router,
};
#[cfg(feature = "advanced-security")]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "advanced-security")]
use tokio::time::sleep;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::{handlers::*, S3View};

#[cfg(feature = "advanced-security")]
use common::security::ebpf_gateway::{EbpfGateway, MtlsLayer, MtlsRejection, ZeroTrustConfig};

pub struct S3Server {
    s3_view: Arc<S3View>,
    port: u16,
    #[cfg(feature = "advanced-security")]
    gateway: Option<EbpfGateway>,
}

impl S3Server {
    pub fn new(s3_view: S3View, port: u16) -> Self {
        #[cfg(feature = "advanced-security")]
        let gateway = Self::init_gateway();
        Self {
            s3_view: Arc::new(s3_view),
            port,
            #[cfg(feature = "advanced-security")]
            gateway,
        }
    }

    pub async fn run(self) -> Result<()> {
        #[cfg(feature = "advanced-security")]
        let gateway = self.gateway.clone();

        // Build router with S3-compatible endpoints
        #[allow(unused_mut)]
        let mut app = Router::new()
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

        #[cfg(feature = "advanced-security")]
        if let Some(gateway) = &gateway {
            let layer = MtlsLayer::new(gateway);
            app = app.layer(from_fn(move |req, next| {
                enforce_mtls(layer.clone(), req, next)
            }));
        }

        #[cfg(feature = "advanced-security")]
        if let Some(gateway) = &gateway {
            if let Some((client, interval)) = gateway.workload_source() {
                let gateway_clone = gateway.clone();
                tokio::spawn(async move {
                    let period = interval;
                    loop {
                        match client.fetch_allowed().await {
                            Ok(ids) if !ids.is_empty() => {
                                gateway_clone.update_allow_list(ids);
                            }
                            Ok(_) => {
                                // No update; keep previous allow-list
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to refresh SPIFFE identities");
                            }
                        }
                        sleep(period).await;
                    }
                });
            }
        }

        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        info!("🚀 SPACE S3 Protocol View listening on http://{}", addr);
        info!("📦 Ready to serve capsules via S3 API");
        info!("");
        info!("Try:");
        info!(
            "  curl -X PUT http://localhost:{}/demo-bucket/hello.txt -d 'Hello SPACE!'",
            self.port
        );
        info!(
            "  curl http://localhost:{}/demo-bucket/hello.txt",
            self.port
        );
        info!("");

        axum::serve(listener, app).await?;

        Ok(())
    }

    #[cfg(feature = "advanced-security")]
    fn init_gateway() -> Option<EbpfGateway> {
        let allowed = std::env::var("SPACE_ALLOWED_SPIFFE_IDS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|id| {
                let trimmed = id.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .collect::<Vec<_>>();
        let header = std::env::var("SPACE_SPIFFE_HEADER").unwrap_or_else(|_| "x-spiffe-id".into());
        let bpf_program = std::env::var("SPACE_BPF_PROGRAM").ok().map(PathBuf::from);
        let spiffe_endpoint = std::env::var("SPACE_SPIFFE_ENDPOINT").ok();
        let refresh_interval_secs = std::env::var("SPACE_SPIFFE_REFRESH_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);

        let config = ZeroTrustConfig {
            bpf_program,
            allowed_spiffe_ids: allowed,
            spiffe_endpoint,
            header_name: header,
            refresh_interval_secs,
        };

        match EbpfGateway::new(config) {
            Ok(gateway) => Some(gateway),
            Err(err) => {
                info!(error = %err, "failed to initialize zero-trust gateway");
                None
            }
        }
    }
}

#[cfg(feature = "advanced-security")]
async fn enforce_mtls(layer: MtlsLayer, mut req: Request<Body>, next: Next) -> Response {
    match layer.authorize(&req) {
        Ok(identity) => {
            req.extensions_mut().insert(identity);
            next.run(req).await
        }
        Err(MtlsRejection { status, message }) => Response::builder()
            .status(status)
            .body(Body::from(message))
            .unwrap(),
    }
}

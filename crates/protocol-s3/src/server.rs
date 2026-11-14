use anyhow::Result;

use axum::{
    extract::{Request, State},
    http::header,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use std::future::IntoFuture;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};

use crate::{handlers::*, S3Error, S3View};
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

/// The hosts a request may name in its `Host` header (#200). The server
/// has no authentication, so its boundary is that it listens on loopback,
/// and that a browser won't show a page the replies to requests it makes
/// to another origin. DNS rebinding gets round the second half: a page
/// points its own name at 127.0.0.1, and its requests to that name are
/// then same-origin. They still carry the name in `Host`, so the server
/// answers only `localhost`, IP addresses, and the names given here.
#[derive(Debug, Clone, Default)]
pub struct AllowedHosts {
    names: Vec<String>,
}

impl AllowedHosts {
    /// Allow `names` as well, such as the DNS names a server bound beyond
    /// loopback is reached by. Case doesn't matter.
    pub fn with_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            names: names.into_iter().map(|n| n.to_ascii_lowercase()).collect(),
        }
    }

    /// Whether `host`, a `Host` header's name or address with or without
    /// a port, is one the server answers. A page can't point an IP address
    /// at this machine the way it can a name, so every address is allowed.
    pub fn allows(&self, host: &str) -> bool {
        if let Some(rest) = host.strip_prefix('[') {
            return rest.split_once(']').is_some_and(|(ip, port)| {
                ip.parse::<Ipv6Addr>().is_ok() && (port.is_empty() || port.starts_with(':'))
            });
        }
        let name = host.rsplit_once(':').map_or(host, |(name, _port)| name);
        name.parse::<Ipv4Addr>().is_ok()
            || name.eq_ignore_ascii_case("localhost")
            || self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }
}

/// Refuse a request whose `Host` the server doesn't answer, or that a web
/// page on another origin sent (#200). A page can still send some requests
/// without the browser asking first, such as a DeleteObjects POST. The
/// browser names the page in `Origin` then, and S3 clients send none, so a
/// request with an `Origin` must come from the origin its `Host` names.
async fn check_host(
    State(allowed): State<Arc<AllowedHosts>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    // Browsers always send Host. HTTP/2 puts it in the request's authority.
    let host = match headers.get(header::HOST) {
        Some(value) => match value.to_str() {
            Ok(host) => Some(host.to_string()),
            Err(_) => return refuse("the Host header isn't ASCII"),
        },
        None => request.uri().authority().map(ToString::to_string),
    };
    if let Some(host) = &host {
        if !allowed.allows(host) {
            return refuse(&format!(
                "this server doesn't answer to the host '{host}'. Address it as localhost or \
                 by IP address, or start serve-s3 with --allow-host for this name"
            ));
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let same_origin = origin
            .to_str()
            .ok()
            .and_then(|origin| origin.split_once("://"))
            .zip(host.as_deref())
            .is_some_and(|((_, authority), host)| authority.eq_ignore_ascii_case(host));
        if !same_origin {
            return refuse("requests from web pages on other origins are refused");
        }
    }
    next.run(request).await
}

fn refuse(why: &str) -> Response {
    S3Error::AccessDenied(why.to_string()).into_response()
}

/// [`router_allowing`] the default hosts: `localhost` and IP addresses.
pub fn router(s3_view: Arc<S3View>) -> Router {
    router_allowing(s3_view, AllowedHosts::default())
}

/// The S3 routes. Keys may contain '/', so the key is a wildcard (#88).
/// A key can't be empty, so `/{bucket}/` is the bucket, as in S3 (#179).
/// The health check is `/-/health`: a new bucket's name can't start with
/// `-`, so it can't hide a bucket (#164). `GET /` is ListBuckets (#159).
/// Anything else, an unrouted path or method, is a `NotImplemented` S3
/// error (#156). A request must name one of `hosts` ([`AllowedHosts`]).
pub fn router_allowing(s3_view: Arc<S3View>, hosts: AllowedHosts) -> Router {
    let bucket = || {
        get(list_objects)
            .put(create_bucket)
            .head(head_bucket)
            .delete(delete_bucket)
            .post(delete_objects)
    };
    Router::new()
        .route("/", get(list_buckets))
        .route("/-/health", get(health_check))
        // Bucket Operations
        .route("/{bucket}", bucket())
        .route("/{bucket}/", bucket())
        // S3 Object Operations
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .put(put_object)
                .head(head_object)
                .delete(delete_object),
        )
        .method_not_allowed_fallback(not_implemented)
        .fallback(not_implemented)
        .with_state(s3_view)
        .layer(middleware::from_fn_with_state(Arc::new(hosts), check_host))
        .layer(TraceLayer::new_for_http())
}

/// The server has no authentication and sends no CORS headers. It answers
/// only requests that name `localhost`, an IP address or an allowed name in
/// `Host`, and refuses requests from web pages on other origins
/// ([`AllowedHosts`]), so a web page can't reach it through the browser.
/// Bind it to a non-loopback address only on a trusted network.
pub struct S3Server {
    s3_view: Arc<S3View>,
    listeners: Vec<TcpListener>,
    reclaim_every: Option<Duration>,
    scrub: Option<ScrubConfig>,
    hosts: AllowedHosts,
}

impl S3Server {
    /// Serve `s3_view` on each of `listeners`. The caller binds them, so it
    /// can fail on a port in use before opening the store.
    pub fn new(s3_view: S3View, listeners: Vec<TcpListener>) -> Self {
        Self {
            s3_view: Arc::new(s3_view),
            listeners,
            reclaim_every: None,
            scrub: None,
            hosts: AllowedHosts::default(),
        }
    }

    /// The hosts requests may name ([`AllowedHosts`]). The default is
    /// `localhost` and IP addresses.
    pub fn allow_hosts(mut self, hosts: AllowedHosts) -> Self {
        self.hosts = hosts;
        self
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
        let app = router_allowing(self.s3_view, self.hosts);

        if let Some(addr) = self.listeners.first().and_then(|l| l.local_addr().ok()) {
            debug!(
                "try: curl -X PUT http://{addr}/demo-bucket && curl -X PUT http://{addr}/demo-bucket/hello.txt --data-binary 'Hello SPACE!'"
            );
        }
        let servers = self
            .listeners
            .into_iter()
            .map(|listener| axum::serve(listener, app.clone()).into_future());
        futures::future::try_join_all(servers).await?;

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

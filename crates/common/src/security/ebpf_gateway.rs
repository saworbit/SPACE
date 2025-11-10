use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use http::{header::HeaderName, Request, StatusCode};
use serde::Deserialize;
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[cfg(target_os = "linux")]
use tracing::info;
use tracing::warn;

#[cfg(target_os = "linux")]
use aya::Bpf;

/// Runtime configuration for the zero-trust ingress stack.
#[derive(Debug, Clone)]
pub struct ZeroTrustConfig {
    pub bpf_program: Option<PathBuf>,
    pub allowed_spiffe_ids: Vec<String>,
    pub spiffe_endpoint: Option<String>,
    pub header_name: String,
    pub refresh_interval_secs: u64,
}

impl Default for ZeroTrustConfig {
    fn default() -> Self {
        Self {
            bpf_program: None,
            allowed_spiffe_ids: Vec::new(),
            spiffe_endpoint: None,
            header_name: "x-spiffe-id".into(),
            refresh_interval_secs: 30,
        }
    }
}

/// Wrapper that loads the eBPF program (when supported) and tracks allowed SPIFFE identities.
#[derive(Clone)]
pub struct EbpfGateway {
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    program: Option<Arc<Bpf>>,
    allowed: Arc<RwLock<HashSet<String>>>,
    header_name: String,
    workload_client: Option<SpiffeWorkloadClient>,
    refresh_interval: Duration,
}

impl EbpfGateway {
    pub fn new(config: ZeroTrustConfig) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let program = if let Some(path) = config.bpf_program.as_ref() {
            match Bpf::load_file(path) {
                Ok(prog) => {
                    info!("loaded eBPF ingress program from {}", path.display());
                    Some(Arc::new(prog))
                }
                Err(err) => {
                    warn!(error = %err, "failed to load eBPF program");
                    None
                }
            }
        } else {
            None
        };

        #[cfg(not(target_os = "linux"))]
        if let Some(path) = config.bpf_program.as_ref() {
            warn!(
                "eBPF ingress is only available on Linux; ignoring program {}",
                path.display()
            );
        }

        Ok(Self {
            #[cfg(target_os = "linux")]
            program,
            allowed: Arc::new(RwLock::new(config.allowed_spiffe_ids.into_iter().collect())),
            header_name: config.header_name.clone(),
            workload_client: config.spiffe_endpoint.map(SpiffeWorkloadClient::new),
            refresh_interval: Duration::from_secs(config.refresh_interval_secs.max(5)),
        })
    }

    pub fn allowed_identities(&self) -> Arc<RwLock<HashSet<String>>> {
        Arc::clone(&self.allowed)
    }

    pub fn header_name(&self) -> &str {
        &self.header_name
    }

    pub fn update_allow_list<I>(&self, ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        let mut guard = self.allowed.write().unwrap();
        guard.clear();
        guard.extend(ids);
    }

    pub fn workload_source(&self) -> Option<(SpiffeWorkloadClient, Duration)> {
        self.workload_client
            .clone()
            .map(|client| (client, self.refresh_interval))
    }
}

/// Extracts SPIFFE identities from requests.
#[derive(Clone)]
pub struct MtlsLayer {
    allowed: Arc<RwLock<HashSet<String>>>,
    header: HeaderName,
}

impl MtlsLayer {
    pub fn new(gateway: &EbpfGateway) -> Self {
        let header = HeaderName::from_bytes(gateway.header_name().as_bytes())
            .unwrap_or_else(|_| HeaderName::from_static("x-spiffe-id"));
        Self {
            allowed: gateway.allowed_identities(),
            header,
        }
    }

    pub fn authorize<B>(&self, req: &Request<B>) -> Result<SpiffeIdentity, MtlsRejection> {
        let header_value = req
            .headers()
            .get(&self.header)
            .ok_or_else(MtlsRejection::missing_identity)?;

        let spiffe = header_value
            .to_str()
            .map_err(|_| MtlsRejection::invalid_identity())?;

        let allowed = self.allowed.read().unwrap();
        if !allowed.is_empty() && !allowed.contains(spiffe) {
            return Err(MtlsRejection::unauthorized(spiffe));
        }

        Ok(SpiffeIdentity {
            value: spiffe.to_string(),
        })
    }
}

/// Simple SPIFFE identity wrapper injected into request extensions.
#[derive(Debug, Clone)]
pub struct SpiffeIdentity {
    value: String,
}

impl SpiffeIdentity {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

#[derive(Debug)]
pub struct MtlsRejection {
    pub status: StatusCode,
    pub message: String,
}

impl MtlsRejection {
    fn missing_identity() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "SPIFFE identity header missing".into(),
        }
    }

    fn invalid_identity() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "SPIFFE identity header malformed".into(),
        }
    }

    fn unauthorized(identity: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: format!("SPIFFE identity {identity} not authorized"),
        }
    }
}

/// Minimal websocket client that talks to a SPIFFE workload API exposed via WebSocket/mTLS tunnel.
#[derive(Clone)]
pub struct SpiffeWorkloadClient {
    endpoint: String,
}

impl SpiffeWorkloadClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    pub async fn fetch_allowed(&self) -> Result<Vec<String>> {
        let (mut socket, _) = connect_async(&self.endpoint)
            .await
            .with_context(|| format!("failed to connect to {}", self.endpoint))?;

        socket.send(Message::Text("IDENTITIES".into())).await.ok();

        while let Some(msg) = socket.next().await {
            match msg {
                Ok(Message::Text(payload)) => {
                    let parsed: IdentitiesPayload =
                        serde_json::from_str(&payload).context("invalid SPIFFE payload")?;
                    return Ok(parsed.allowed);
                }
                Ok(Message::Binary(_)) => continue,
                Ok(Message::Close(_)) => break,
                Err(err) => {
                    warn!(error = %err, "spiffe stream error");
                    break;
                }
                _ => continue,
            }
        }

        Ok(Vec::new())
    }
}

#[derive(Deserialize)]
struct IdentitiesPayload {
    #[serde(default)]
    allowed: Vec<String>,
}

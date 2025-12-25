use crate::rpc::{
    federation_service_server::FederationService, CapsuleMetadata, HelloRequest, HelloResponse,
    RegisterAck, SegmentChunk, TransferAck,
};
use crate::transport::RaftServiceImpl;
use anyhow::{Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{Capsule, CapsuleId, ContentHash, Policy, SegmentId};
use nvram_sim::NvramLog;
use raft::prelude::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing::info;

#[derive(Clone)]
pub struct FederationServiceImpl {
    registry: Arc<CapsuleRegistry>,
    nvram: Arc<NvramLog>,
    expected_secret: Option<String>,
}

impl FederationServiceImpl {
    pub fn new(
        registry: Arc<CapsuleRegistry>,
        nvram: Arc<NvramLog>,
        expected_secret: Option<String>,
    ) -> Self {
        Self {
            registry,
            nvram,
            expected_secret,
        }
    }

    #[allow(clippy::result_large_err)]
    fn authenticate(&self, secret: &str) -> Result<(), Status> {
        let Some(expected) = self.expected_secret.as_deref() else {
            return Ok(());
        };
        if expected == secret {
            Ok(())
        } else {
            Err(Status::unauthenticated("invalid federation secret"))
        }
    }

    #[allow(clippy::result_large_err)]
    fn authenticate_metadata(&self, metadata: &tonic::metadata::MetadataMap) -> Result<(), Status> {
        let Some(expected) = self.expected_secret.as_deref() else {
            return Ok(());
        };
        let provided = metadata
            .get("x-space-secret")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided == expected {
            Ok(())
        } else {
            Err(Status::unauthenticated("missing/invalid federation secret"))
        }
    }
}

#[tonic::async_trait]
impl FederationService for FederationServiceImpl {
    async fn hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloResponse>, Status> {
        let req = request.into_inner();
        self.authenticate(&req.secret)?;
        Ok(Response::new(HelloResponse {
            ok: true,
            message: format!("hello {}", req.zone_id),
        }))
    }

    async fn push_segment(
        &self,
        request: Request<tonic::Streaming<SegmentChunk>>,
    ) -> Result<Response<TransferAck>, Status> {
        self.authenticate_metadata(request.metadata())?;
        let mut stream = request.into_inner();

        let mut capsule_id = String::new();
        let mut segment_index: u32 = 0;
        let mut expected_hash: Option<Vec<u8>> = None;
        let mut total_len: Option<u64> = None;
        let mut bytes: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.message().await? {
            if capsule_id.is_empty() {
                capsule_id = chunk.capsule_id.clone();
                segment_index = chunk.segment_index;
            }
            if expected_hash.is_none() && !chunk.content_hash.is_empty() {
                expected_hash = Some(chunk.content_hash.clone());
            }
            if total_len.is_none() && chunk.total_len > 0 {
                total_len = Some(chunk.total_len);
            }
            bytes.extend_from_slice(&chunk.data);
        }

        if capsule_id.is_empty() {
            return Err(Status::invalid_argument("missing capsule_id"));
        }

        if let Some(total) = total_len {
            if bytes.len() as u64 != total {
                return Err(Status::invalid_argument("segment length mismatch"));
            }
        }

        let computed = blake3::hash(&bytes);
        if let Some(expected) = expected_hash {
            if expected != computed.as_bytes() {
                return Err(Status::invalid_argument("segment hash mismatch"));
            }
        }

        let hash = ContentHash::from_bytes(computed.as_bytes());
        if let Some(existing) = self.registry.lookup_content(&hash) {
            return Ok(Response::new(TransferAck {
                ok: true,
                segment_id: existing.0,
                message: "segment already present".into(),
            }));
        }

        let seg_id = self
            .registry
            .alloc_segment()
            .map_err(|e| Status::internal(e.to_string()))?;
        let mut segment = self
            .nvram
            .append(seg_id, &bytes)
            .map_err(|e| Status::internal(e.to_string()))?;
        segment.content_hash = Some(hash.clone());
        self.nvram
            .update_segment_metadata(seg_id, segment)
            .map_err(|e| Status::internal(e.to_string()))?;
        self.registry
            .register_content(hash, seg_id)
            .map_err(|e| Status::internal(e.to_string()))?;

        info!(
            capsule = %capsule_id,
            segment_index,
            seg_id = seg_id.0,
            bytes = bytes.len(),
            "federation segment stored"
        );

        Ok(Response::new(TransferAck {
            ok: true,
            segment_id: seg_id.0,
            message: "stored".into(),
        }))
    }

    async fn register_capsule(
        &self,
        request: Request<CapsuleMetadata>,
    ) -> Result<Response<RegisterAck>, Status> {
        self.authenticate_metadata(request.metadata())?;
        let meta = request.into_inner();
        let uuid = meta
            .capsule_id
            .parse()
            .map_err(|e| Status::invalid_argument(format!("invalid capsule_id: {e}")))?;
        let capsule_id = CapsuleId::from_uuid(uuid);

        let policy = if meta.policy_json.is_empty() {
            Policy::default()
        } else {
            serde_json::from_slice(&meta.policy_json)
                .map_err(|e| Status::invalid_argument(format!("invalid policy_json: {e}")))?
        };

        let segments: Vec<SegmentId> = meta.segment_ids.into_iter().map(SegmentId).collect();
        let capsule = Capsule {
            id: capsule_id,
            size: meta.size,
            segments,
            created_at: meta.created_at,
            policy,
            deduped_bytes: meta.deduped_bytes,
        };

        let inserted = self
            .registry
            .put_capsule(capsule)
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(RegisterAck {
            ok: true,
            already_exists: !inserted,
            message: if inserted {
                "registered".into()
            } else {
                "already exists".into()
            },
        }))
    }
}

pub async fn serve(
    addr: SocketAddr,
    registry: Arc<CapsuleRegistry>,
    nvram: Arc<NvramLog>,
    expected_secret: Option<String>,
) -> Result<()> {
    let service = FederationServiceImpl::new(registry, nvram, expected_secret);
    tonic::transport::Server::builder()
        .add_service(crate::rpc::federation_service_server::FederationServiceServer::new(service))
        .serve(addr)
        .await
        .context("serve federation gRPC")?;
    Ok(())
}

pub async fn serve_from_paths(
    addr: SocketAddr,
    metadata_path: &str,
    nvram_path: &str,
    expected_secret: Option<String>,
) -> Result<()> {
    let registry = Arc::new(
        CapsuleRegistry::open(metadata_path)
            .with_context(|| format!("open registry at {}", metadata_path))?,
    );
    let nvram = Arc::new(
        NvramLog::open(nvram_path).with_context(|| format!("open nvram at {}", nvram_path))?,
    );
    serve(addr, registry, nvram, expected_secret).await
}

/// Serve both FederationService and RaftService on the same port.
///
/// This is the production entry point that runs both the data plane
/// (FederationService for segment transfer) and control plane
/// (RaftService for consensus messages) on a single gRPC server.
///
/// # Arguments
/// - `addr`: The address to bind to
/// - `registry`: Capsule metadata registry
/// - `nvram`: NVRAM log for segments
/// - `expected_secret`: Optional authentication secret for FederationService
/// - `raft_inbox`: Channel to send received Raft messages to the RaftEngine
///
/// # Example
/// ```ignore
/// let (raft_inbox_tx, raft_inbox_rx) = mpsc::channel(100);
/// let registry = Arc::new(CapsuleRegistry::open("./registry")?);
/// let nvram = Arc::new(NvramLog::open("./nvram")?);
///
/// serve_with_raft(
///     "127.0.0.1:4422".parse()?,
///     registry,
///     nvram,
///     None,
///     raft_inbox_tx,
/// ).await?;
/// ```
pub async fn serve_with_raft(
    addr: SocketAddr,
    registry: Arc<CapsuleRegistry>,
    nvram: Arc<NvramLog>,
    expected_secret: Option<String>,
    raft_inbox: mpsc::Sender<Message>,
) -> Result<()> {
    let federation_service = FederationServiceImpl::new(registry, nvram, expected_secret);
    let raft_service = RaftServiceImpl::new(raft_inbox);

    info!(addr = %addr, "starting dual gRPC server (FederationService + RaftService)");

    tonic::transport::Server::builder()
        .add_service(
            crate::rpc::federation_service_server::FederationServiceServer::new(federation_service),
        )
        .add_service(crate::rpc::raft_service_server::RaftServiceServer::new(
            raft_service,
        ))
        .serve(addr)
        .await
        .context("serve dual gRPC server")?;

    Ok(())
}

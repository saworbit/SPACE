use mesh_core::{GossipStats, NodeRole, Peer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use validator::Validate;

/// The standard API response envelope.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse<T>
where
    T: Serialize,
{
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<ApiErrorBody>,
    pub meta: Option<Meta>,
}

impl<T> ApiResponse<T>
where
    T: Serialize + ToSchema,
{
    pub fn success(data: T, meta: Option<Meta>) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            meta,
        }
    }

    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ApiErrorBody {
                code: code.into(),
                message: message.into(),
                request_id: request_id.into(),
            }),
            meta: None,
        }
    }
}

/// Machine and human readable error details for envelopes.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

/// Optional metadata for pagination and tracing.
#[derive(Debug, Serialize, Deserialize, Default, ToSchema, Clone)]
pub struct Meta {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub total: Option<u64>,
    pub sort: Option<String>,
    pub trace_id: Option<String>,
}

impl Meta {
    pub fn with_trace_id(mut self, trace_id: Option<String>) -> Self {
        self.trace_id = trace_id;
        self
    }

    pub fn with_pagination(
        mut self,
        page: u32,
        limit: u32,
        total: u64,
        sort: Option<String>,
    ) -> Self {
        self.page = Some(page);
        self.limit = Some(limit);
        self.total = Some(total);
        self.sort = sort;
        self
    }
}

/// Standard pagination and cursor query params.
#[derive(Debug, Deserialize, Validate, ToSchema, Default, Clone, IntoParams)]
pub struct PaginationQuery {
    #[validate(range(min = 1))]
    pub page: Option<u32>,

    #[validate(range(min = 1, max = 500))]
    pub limit: Option<u32>,

    pub sort: Option<String>,

    /// Cursor-based continuation token
    pub after_id: Option<String>,
}

/// RBAC roles exposed to the API.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    Admin,
    Editor,
    Viewer,
}

/// Claims injected by JWT middleware.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct Claims {
    pub sub: String,
    pub role: UserRole,
    pub exp: usize,
    pub iat: Option<usize>,
}

/// Node roles used inside mesh responses and requests.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeshRole {
    Admin,
    Viewer,
    Editor,
    StorageNode,
    Gateway,
}

impl From<MeshRole> for NodeRole {
    fn from(role: MeshRole) -> Self {
        match role {
            MeshRole::Admin => NodeRole::Admin,
            MeshRole::Viewer => NodeRole::Viewer,
            MeshRole::Editor => NodeRole::Editor,
            MeshRole::StorageNode => NodeRole::StorageNode,
            MeshRole::Gateway => NodeRole::Gateway,
        }
    }
}

impl From<NodeRole> for MeshRole {
    fn from(role: NodeRole) -> Self {
        match role {
            NodeRole::Admin => MeshRole::Admin,
            NodeRole::Viewer => MeshRole::Viewer,
            NodeRole::Editor => MeshRole::Editor,
            NodeRole::StorageNode => MeshRole::StorageNode,
            NodeRole::Gateway => MeshRole::Gateway,
        }
    }
}

/// Slim peer view for API responses.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PeerView {
    pub id: String,
    pub addr: String,
    pub role: MeshRole,
    pub storage_usage: u64,
    pub status: String,
    pub gossip_version: u32,
    pub last_gossip_heartbeat: u64,
}

impl From<Peer> for PeerView {
    fn from(peer: Peer) -> Self {
        Self {
            id: peer.id,
            addr: peer.addr.to_string(),
            role: peer.role.into(),
            storage_usage: peer.storage_usage,
            status: peer.status,
            gossip_version: peer.gossip_version,
            last_gossip_heartbeat: peer.last_gossip_heartbeat,
        }
    }
}

/// Peer listing response with metrics.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PeersResponse {
    pub peers: Vec<PeerView>,
    pub gossip_metrics: HashMap<String, f64>,
    pub total_count: usize,
}

/// Gossip stats tailored for OpenAPI schemas.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct GossipStatsView {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub avg_convergence_ms: f64,
    pub duplication_rate: f64,
    pub active_topics: usize,
    pub connected_peers: usize,
    pub bandwidth_usage: u64,
}

impl From<GossipStats> for GossipStatsView {
    fn from(stats: GossipStats) -> Self {
        Self {
            messages_sent: stats.messages_sent,
            messages_received: stats.messages_received,
            avg_convergence_ms: stats.avg_convergence_ms,
            duplication_rate: stats.duplication_rate,
            active_topics: stats.active_topics,
            connected_peers: stats.connected_peers,
            bandwidth_usage: stats.bandwidth_usage,
        }
    }
}

/// Gossip statistics response.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct StatsResponse {
    pub stats: GossipStatsView,
    pub additional: HashMap<String, serde_json::Value>,
}

/// Graph representation of the mesh.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct TopologyNode {
    pub id: String,
    pub role: MeshRole,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct TopologyEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct TopologyResponse {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
}

/// Request to connect to a peer manually.
#[derive(Debug, Deserialize, Validate, ToSchema, Clone)]
pub struct ConnectPeerRequest {
    #[validate(length(min = 1))]
    pub peer_id: String,

    #[validate(length(min = 1))]
    pub address: String,

    pub role: MeshRole,
}

/// Response acknowledging mesh control operations.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct MeshActionResponse {
    pub peer_id: String,
    pub message: String,
    pub accepted: bool,
}

/// File listing item.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct FileListItem {
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub uploaded_at: u64,
}

/// File listing response.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct FilesListResponse {
    pub files: Vec<FileListItem>,
    pub total: usize,
    pub total_size: u64,
}

/// Upload success payload.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct FileUploadResponse {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub uploader: String,
}

/// Multipart form metadata for uploads.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct UploadRequest {
    pub path: Option<String>,
}

/// Object metadata for HEAD or listings.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ObjectMetadata {
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub uploaded_at: u64,
}

/// Gossip publish request.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PublishRequest {
    pub topic: String,
    pub payload: Vec<u8>,
}

/// Gossip publish response.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PublishResponse {
    pub topic: String,
    pub routed: bool,
}

/// Active subscription info.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct SubscriptionInfo {
    pub topic: String,
    pub receivers: usize,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct SubscriptionsResponse {
    pub subscriptions: Vec<SubscriptionInfo>,
}

/// Health status response.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_ms: u128,
}

/// System metadata response.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct SystemInfo {
    pub version: String,
    pub node_id: String,
    pub uptime_ms: u128,
    pub features: Vec<String>,
    pub peer_count: usize,
}

/// Per-request context injected by middleware.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: String,
}

// OpenAPI-friendly concrete envelopes for generic ApiResponse<T>.
macro_rules! api_response_schema {
    ($name:ident, $inner:ty) => {
        #[derive(Debug, Serialize, ToSchema, Clone)]
        pub struct $name {
            pub success: bool,
            pub data: Option<$inner>,
            pub error: Option<ApiErrorBody>,
            pub meta: Option<Meta>,
        }
    };
}

api_response_schema!(ApiResponsePeersResponseSchema, PeersResponse);
api_response_schema!(ApiResponsePeerViewSchema, PeerView);
api_response_schema!(ApiResponseFilesListSchema, FilesListResponse);
api_response_schema!(ApiResponseFileUploadSchema, FileUploadResponse);
api_response_schema!(ApiResponseMeshActionSchema, MeshActionResponse);
api_response_schema!(ApiResponseTopologySchema, TopologyResponse);
api_response_schema!(ApiResponseStatsSchema, StatsResponse);
api_response_schema!(ApiResponseSubscriptionsSchema, SubscriptionsResponse);
api_response_schema!(ApiResponseSystemInfoSchema, SystemInfo);
api_response_schema!(ApiResponseHealthStatusSchema, HealthStatus);
api_response_schema!(ApiResponsePublishSchema, PublishResponse);

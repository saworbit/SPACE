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
    pub iss: Option<String>,
    pub scope: Option<Vec<String>>,
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
    /// Per-subsystem health check results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<HealthCheck>,
}

/// Individual health check result with severity classification.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct HealthCheck {
    /// Unique check identifier, e.g. `GOSSIP_OK`, `PEER_COUNT_LOW`.
    pub id: String,
    /// `ok`, `warn`, or `error`.
    pub severity: String,
    /// Human-readable detail.
    pub message: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::{GossipStats, NodeRole, Peer};
    use std::net::SocketAddr;

    // ── ApiResponse::success ──────────────────────────────────────

    #[test]
    fn api_response_success() {
        let resp = ApiResponse::success("hello".to_string(), None);
        assert!(resp.success);
        assert_eq!(resp.data.as_deref(), Some("hello"));
        assert!(resp.error.is_none());
    }

    #[test]
    fn api_response_success_with_meta() {
        let meta = Meta {
            trace_id: Some("trace-1".into()),
            ..Default::default()
        };
        let resp = ApiResponse::success(42u64, Some(meta));
        assert!(resp.success);
        assert_eq!(resp.data, Some(42));
        assert_eq!(
            resp.meta.as_ref().unwrap().trace_id.as_deref(),
            Some("trace-1")
        );
    }

    // ── ApiResponse::error ────────────────────────────────────────

    #[test]
    fn api_response_error() {
        let resp = ApiResponse::<String>::error("NOT_FOUND", "missing", "req-1");
        assert!(!resp.success);
        assert!(resp.data.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "NOT_FOUND");
        assert_eq!(err.message, "missing");
        assert_eq!(err.request_id, "req-1");
    }

    // ── Meta builders ─────────────────────────────────────────────

    #[test]
    fn meta_default_is_empty() {
        let m = Meta::default();
        assert!(m.page.is_none());
        assert!(m.limit.is_none());
        assert!(m.total.is_none());
        assert!(m.sort.is_none());
        assert!(m.trace_id.is_none());
    }

    #[test]
    fn meta_with_trace_id() {
        let m = Meta::default().with_trace_id(Some("abc".into()));
        assert_eq!(m.trace_id.as_deref(), Some("abc"));
    }

    #[test]
    fn meta_with_pagination() {
        let m = Meta::default().with_pagination(2, 25, 100, Some("name".into()));
        assert_eq!(m.page, Some(2));
        assert_eq!(m.limit, Some(25));
        assert_eq!(m.total, Some(100));
        assert_eq!(m.sort.as_deref(), Some("name"));
    }

    // ── UserRole serde ────────────────────────────────────────────

    #[test]
    fn user_role_serde_roundtrip() {
        for role in [UserRole::Admin, UserRole::Editor, UserRole::Viewer] {
            let json = serde_json::to_string(&role).unwrap();
            let restored: UserRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, restored);
        }
    }

    #[test]
    fn user_role_serde_snake_case() {
        let json = serde_json::to_string(&UserRole::Admin).unwrap();
        assert!(json.contains("admin"), "should use snake_case: {json}");
    }

    // ── MeshRole <-> NodeRole conversions ─────────────────────────

    #[test]
    fn mesh_role_to_node_role() {
        let cases = [
            (MeshRole::Admin, NodeRole::Admin),
            (MeshRole::Viewer, NodeRole::Viewer),
            (MeshRole::Editor, NodeRole::Editor),
            (MeshRole::StorageNode, NodeRole::StorageNode),
            (MeshRole::Gateway, NodeRole::Gateway),
        ];
        for (mesh, expected_node) in cases {
            let nr: NodeRole = mesh.into();
            assert_eq!(nr, expected_node);
        }
    }

    #[test]
    fn node_role_to_mesh_role() {
        let cases = [
            (NodeRole::Admin, MeshRole::Admin),
            (NodeRole::Viewer, MeshRole::Viewer),
            (NodeRole::Editor, MeshRole::Editor),
            (NodeRole::StorageNode, MeshRole::StorageNode),
            (NodeRole::Gateway, MeshRole::Gateway),
        ];
        for (node, expected_mesh) in cases {
            let mr: MeshRole = node.into();
            assert_eq!(mr, expected_mesh);
        }
    }

    // ── PeerView::from(Peer) ──────────────────────────────────────

    #[test]
    fn peer_view_from_peer() {
        let addr: SocketAddr = "10.0.0.1:9090".parse().unwrap();
        let mut peer = Peer::new("node-42".into(), addr, NodeRole::StorageNode);
        peer.storage_usage = 1024;
        peer.gossip_version = 5;
        peer.last_gossip_heartbeat = 999;

        let view = PeerView::from(peer);
        assert_eq!(view.id, "node-42");
        assert_eq!(view.addr, "10.0.0.1:9090");
        assert_eq!(view.role, MeshRole::StorageNode);
        assert_eq!(view.storage_usage, 1024);
        assert_eq!(view.gossip_version, 5);
        assert_eq!(view.last_gossip_heartbeat, 999);
    }

    // ── GossipStatsView::from(GossipStats) ────────────────────────

    #[test]
    fn gossip_stats_view_from_stats() {
        let stats = GossipStats {
            messages_sent: 100,
            messages_received: 90,
            avg_convergence_ms: 1.5,
            duplication_rate: 0.02,
            active_topics: 3,
            connected_peers: 10,
            bandwidth_usage: 4096,
        };
        let view = GossipStatsView::from(stats);
        assert_eq!(view.messages_sent, 100);
        assert_eq!(view.messages_received, 90);
        assert!((view.avg_convergence_ms - 1.5).abs() < f64::EPSILON);
        assert!((view.duplication_rate - 0.02).abs() < f64::EPSILON);
        assert_eq!(view.active_topics, 3);
        assert_eq!(view.connected_peers, 10);
        assert_eq!(view.bandwidth_usage, 4096);
    }

    // ── Claims serde roundtrip ────────────────────────────────────

    #[test]
    fn claims_serde_roundtrip() {
        let claims = Claims {
            sub: "alice".into(),
            role: UserRole::Editor,
            exp: 99999,
            iss: Some("space-auth".into()),
            scope: Some(vec!["read".into(), "write".into()]),
            iat: Some(10000),
        };
        let json = serde_json::to_string(&claims).unwrap();
        let restored: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sub, "alice");
        assert_eq!(restored.role, UserRole::Editor);
        assert_eq!(restored.exp, 99999);
        assert_eq!(restored.iss.as_deref(), Some("space-auth"));
        assert_eq!(restored.scope.as_ref().unwrap().len(), 2);
        assert_eq!(restored.iat, Some(10000));
    }

    // ── PaginationQuery validation ────────────────────────────────

    #[test]
    fn pagination_query_valid() {
        let pq = PaginationQuery {
            page: Some(1),
            limit: Some(50),
            sort: None,
            after_id: None,
        };
        assert!(pq.validate().is_ok());
    }

    #[test]
    fn pagination_query_page_zero_invalid() {
        let pq = PaginationQuery {
            page: Some(0),
            limit: Some(10),
            sort: None,
            after_id: None,
        };
        assert!(pq.validate().is_err());
    }

    #[test]
    fn pagination_query_limit_too_high() {
        let pq = PaginationQuery {
            page: Some(1),
            limit: Some(501),
            sort: None,
            after_id: None,
        };
        assert!(pq.validate().is_err());
    }

    #[test]
    fn pagination_query_defaults_valid() {
        let pq = PaginationQuery::default();
        assert!(pq.validate().is_ok());
    }

    // ── ConnectPeerRequest validation ─────────────────────────────

    #[test]
    fn connect_peer_request_valid() {
        let req = ConnectPeerRequest {
            peer_id: "node-1".into(),
            address: "127.0.0.1:8080".into(),
            role: MeshRole::StorageNode,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn connect_peer_request_empty_peer_id() {
        let req = ConnectPeerRequest {
            peer_id: "".into(),
            address: "127.0.0.1:8080".into(),
            role: MeshRole::Viewer,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn connect_peer_request_empty_address() {
        let req = ConnectPeerRequest {
            peer_id: "node-1".into(),
            address: "".into(),
            role: MeshRole::Viewer,
        };
        assert!(req.validate().is_err());
    }

    // ── MeshRole serde roundtrip ──────────────────────────────────

    #[test]
    fn mesh_role_serde_roundtrip() {
        for role in [
            MeshRole::Admin,
            MeshRole::Viewer,
            MeshRole::Editor,
            MeshRole::StorageNode,
            MeshRole::Gateway,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let restored: MeshRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, restored);
        }
    }

    // ── HealthStatus serde ────────────────────────────────────────

    #[test]
    fn health_status_serde_roundtrip() {
        let hs = HealthStatus {
            status: "ok".into(),
            uptime_ms: 42000,
            checks: vec![HealthCheck {
                id: "TEST".into(),
                severity: "ok".into(),
                message: "all good".into(),
            }],
        };
        let json = serde_json::to_string(&hs).unwrap();
        let restored: HealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.status, "ok");
        assert_eq!(restored.uptime_ms, 42000);
        assert_eq!(restored.checks.len(), 1);
    }

    // ── SystemInfo serde ──────────────────────────────────────────

    #[test]
    fn system_info_serde_roundtrip() {
        let si = SystemInfo {
            version: "0.1.0".into(),
            node_id: "node-abc".into(),
            uptime_ms: 5000,
            features: vec!["gossip".into(), "storage".into()],
            peer_count: 3,
        };
        let json = serde_json::to_string(&si).unwrap();
        let restored: SystemInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, "0.1.0");
        assert_eq!(restored.features.len(), 2);
        assert_eq!(restored.peer_count, 3);
    }

    // ── TopologyResponse ──────────────────────────────────────────

    #[test]
    fn topology_response_serde() {
        let topo = TopologyResponse {
            nodes: vec![TopologyNode {
                id: "n1".into(),
                role: MeshRole::Gateway,
                status: "online".into(),
            }],
            edges: vec![TopologyEdge {
                from: "n1".into(),
                to: "n2".into(),
            }],
        };
        let json = serde_json::to_string(&topo).unwrap();
        let restored: TopologyResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.nodes.len(), 1);
        assert_eq!(restored.edges.len(), 1);
        assert_eq!(restored.nodes[0].id, "n1");
    }

    // ── PublishRequest / PublishResponse ───────────────────────────

    #[test]
    fn publish_request_serde() {
        let req = PublishRequest {
            topic: "events".into(),
            payload: vec![1, 2, 3],
        };
        let json = serde_json::to_string(&req).unwrap();
        let restored: PublishRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.topic, "events");
        assert_eq!(restored.payload, vec![1, 2, 3]);
    }

    #[test]
    fn publish_response_serde() {
        let resp = PublishResponse {
            topic: "events".into(),
            routed: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let restored: PublishResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.topic, "events");
        assert!(restored.routed);
    }

    // ── SubscriptionsResponse ─────────────────────────────────────

    #[test]
    fn subscriptions_response_serde() {
        let resp = SubscriptionsResponse {
            subscriptions: vec![SubscriptionInfo {
                topic: "t1".into(),
                receivers: 5,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let restored: SubscriptionsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.subscriptions.len(), 1);
        assert_eq!(restored.subscriptions[0].receivers, 5);
    }

    // ── FileListItem / FilesListResponse ──────────────────────────

    #[test]
    fn files_list_response_serde() {
        let resp = FilesListResponse {
            files: vec![FileListItem {
                path: "/data/test.bin".into(),
                size: 1024,
                hash: "abc".into(),
                uploaded_at: 12345,
            }],
            total: 1,
            total_size: 1024,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let restored: FilesListResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.total, 1);
        assert_eq!(restored.files[0].path, "/data/test.bin");
    }

    // ── ObjectMetadata ────────────────────────────────────────────

    #[test]
    fn object_metadata_serde() {
        let md = ObjectMetadata {
            path: "/a/b.txt".into(),
            size: 512,
            hash: "def".into(),
            uploaded_at: 99,
        };
        let json = serde_json::to_string(&md).unwrap();
        let restored: ObjectMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.size, 512);
    }
}

use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::api::{
    handlers::{data, gossip, mesh, system},
    models,
};
use crate::state::AppState;

#[derive(OpenApi)]
#[openapi(
    paths(
        system::health,
        system::info,
        system::metrics,
        mesh::list_peers,
        mesh::get_peer,
        mesh::topology,
        mesh::connect_peer,
        mesh::remove_peer,
        data::list_objects,
        data::upload_object,
        data::download_object,
        data::head_object,
        gossip::publish,
        gossip::subscriptions,
        gossip::stats
    ),
    components(
        schemas(
            models::ApiErrorBody,
            models::Meta,
            models::PaginationQuery,
            models::UserRole,
            models::MeshRole,
            models::Claims,
            models::PeerView,
            models::PeersResponse,
            models::TopologyNode,
            models::TopologyEdge,
            models::TopologyResponse,
            models::ConnectPeerRequest,
            models::MeshActionResponse,
            models::FileListItem,
            models::FilesListResponse,
            models::FileUploadResponse,
            models::UploadRequest,
            models::ObjectMetadata,
            models::PublishRequest,
            models::PublishResponse,
            models::SubscriptionsResponse,
            models::SystemInfo,
            models::HealthStatus,
            models::StatsResponse,
            models::GossipStatsView
        )
    ),
    tags(
        (name = "System", description = "System domain endpoints"),
        (name = "Mesh", description = "Mesh control endpoints"),
        (name = "Data", description = "Data plane endpoints"),
        (name = "Gossip", description = "Gossip and event distribution endpoints")
    )
)]
pub struct ApiDoc;

/// Build Swagger UI routes bound to the generated OpenAPI spec.
pub fn swagger_routes() -> Router<AppState> {
    SwaggerUi::new("/swagger-ui")
        .url("/api-docs/openapi.json", ApiDoc::openapi())
        .into()
}

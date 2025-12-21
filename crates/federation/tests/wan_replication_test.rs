#![cfg(feature = "phase4")]

use capsule_registry::pipeline::WritePipeline;
use capsule_registry::CapsuleRegistry;
use common::{Policy, TransferPriority};
use federation::rpc::federation_service_server::FederationServiceServer;
use federation::server::FederationServiceImpl;
use federation::wan::{PeerClientManager, WanTransferAgent};
use federation::zones::ZoneConfig;
use nvram_sim::NvramLog;
use std::sync::Arc;
use tempfile::TempDir;
use tokio_stream::wrappers::TcpListenerStream;

#[tokio::test]
async fn replicates_capsule_over_grpc() {
    let src_dir = TempDir::new().unwrap();
    let src_registry_path = src_dir.path().join("space.db");
    let src_nvram_path = src_dir.path().join("space.nvram");

    let src_registry = CapsuleRegistry::open(&src_registry_path).unwrap();
    let src_nvram = NvramLog::open(&src_nvram_path).unwrap();
    let pipeline = WritePipeline::new(src_registry.clone(), src_nvram.clone());

    let capsule_id = pipeline
        .write_capsule_with_policy(b"hello world", &Policy::default())
        .await
        .unwrap();

    let dst_dir = TempDir::new().unwrap();
    let dst_registry_path = dst_dir.path().join("space.db");
    let dst_nvram_path = dst_dir.path().join("space.nvram");

    let dst_registry = Arc::new(CapsuleRegistry::open(&dst_registry_path).unwrap());
    let dst_nvram = Arc::new(NvramLog::open(&dst_nvram_path).unwrap());

    let secret = "test-secret".to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_registry = Arc::clone(&dst_registry);
    let server_nvram = Arc::clone(&dst_nvram);
    let server = tokio::spawn(async move {
        let service = FederationServiceImpl::new(server_registry, server_nvram, Some(secret));
        tonic::transport::Server::builder()
            .add_service(FederationServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let zone = ZoneConfig {
        name: "zone-2".into(),
        endpoint: format!("http://{}", addr),
        secret_key: "test-secret".into(),
    };

    let agent = WanTransferAgent::default();
    let peers = PeerClientManager::new("zone-1");
    agent
        .replicate_capsule(
            capsule_id,
            &src_registry,
            &src_nvram,
            &peers,
            &zone,
            TransferPriority::Background,
        )
        .await
        .unwrap();

    // Verify data arrived.
    let dst_pipeline =
        WritePipeline::new(dst_registry.as_ref().clone(), dst_nvram.as_ref().clone());
    let bytes = dst_pipeline.read_capsule(capsule_id).await.unwrap();
    assert_eq!(bytes, b"hello world");

    // Re-run replication to confirm idempotency.
    agent
        .replicate_capsule(
            capsule_id,
            &src_registry,
            &src_nvram,
            &peers,
            &zone,
            TransferPriority::Background,
        )
        .await
        .unwrap();

    let bytes = dst_pipeline.read_capsule(capsule_id).await.unwrap();
    assert_eq!(bytes, b"hello world");

    server.abort();
}

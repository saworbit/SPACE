use bytes::Bytes;
use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use futures::stream;
use nvram_sim::NvramLog;
use protocol_s3::{server, S3View};
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use test_support::TempDir;
use tokio::net::TcpListener;

#[tokio::test]
async fn streaming_upload_round_trip() {
    let (_dir, log_path, meta_path) = temp_paths("streaming_upload");

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let view =
        Arc::new(S3View::new(registry, nvram, &StoreSecurity::none()).expect("open S3 view"));
    view.create_bucket("stream-bucket").unwrap();

    let app = server::router(view);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::serve(listener, app.into_make_service());
    let server_handle = tokio::spawn(async move {
        let _ = server.await;
    });

    let client = Client::new();
    let put_url = format!("http://{}/stream-bucket/streamed.bin", addr);

    // 5 x 1MiB chunks
    let upload_stream = stream::iter(
        (0..5).map(|_| Ok::<Bytes, std::io::Error>(Bytes::from(vec![7u8; 1_048_576]))),
    );

    let put_resp = client
        .put(&put_url)
        .body(reqwest::Body::wrap_stream(upload_stream))
        .send()
        .await
        .unwrap();

    assert_eq!(put_resp.status(), StatusCode::OK);

    let get_resp = client.get(&put_url).send().await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = get_resp.bytes().await.unwrap();
    assert_eq!(body.len(), 5 * 1_048_576);

    server_handle.abort();
}

/// A scratch directory, and the log and registry paths inside it.
fn temp_paths(prefix: &str) -> (TempDir, String, String) {
    let dir = TempDir::new(prefix);
    let log = dir.join("space.nvram").to_string_lossy().into_owned();
    let meta = dir.join("space.metadata").to_string_lossy().into_owned();
    (dir, log, meta)
}

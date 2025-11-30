use axum::{routing::put, Router};
use bytes::Bytes;
use capsule_registry::CapsuleRegistry;
use futures::stream;
use nvram_sim::NvramLog;
use protocol_s3::{handlers, S3View};
use reqwest::{Client, StatusCode};
use std::{fs, path::Path, sync::Arc};
use tokio::net::TcpListener;
use uuid::Uuid;

#[tokio::test]
async fn streaming_upload_round_trip() {
    std::env::set_var("SPACE_DISABLE_MODULAR_PIPELINE", "1");

    let (log_path, meta_path) = temp_paths("streaming_upload");
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let view = Arc::new(S3View::new(registry, nvram));

    let app = Router::new()
        .route(
            "/:bucket/:key",
            put(handlers::put_object).get(handlers::get_object),
        )
        .with_state(view);

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
    cleanup_paths(&log_path);
    cleanup_paths(&meta_path);
}

fn temp_paths(prefix: &str) -> (String, String) {
    let base = std::env::temp_dir().join("space_s3_streaming_tests");
    let _ = fs::create_dir_all(&base);
    let unique = format!("{}_{}", prefix, Uuid::new_v4());
    let log = base.join(format!("{unique}.nvram"));
    let meta = base.join(format!("{unique}.metadata"));
    (
        log.to_string_lossy().to_string(),
        meta.to_string_lossy().to_string(),
    )
}

fn cleanup_paths(path: &str) {
    cleanup_path(path);
    cleanup_path(&format!("{}.segments", path));
}

fn cleanup_path(path: &str) {
    let p = Path::new(path);
    match fs::remove_file(p) {
        Ok(_) => (),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            if err.kind() == std::io::ErrorKind::IsADirectory {
                let _ = fs::remove_dir_all(p);
            }
        }
    }
}

//! An encrypted object on a server without `SPACE_MASTER_KEY` is an error
//! response, not its ciphertext with a 200 (#155).
//!
//! This binary holds a single test because it changes a process-wide
//! environment variable.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use capsule_registry::modular_pipeline::registry_pipeline_from_env;
use capsule_registry::CapsuleRegistry;
use protocol_s3::{server, S3View};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

fn router(nvram: &Path, registry: &CapsuleRegistry) -> Router {
    let handle = registry_pipeline_from_env(nvram, registry.clone()).unwrap();
    server::router(Arc::new(S3View::new_modular(handle)))
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body.to_vec())
}

#[tokio::test]
async fn encrypted_get_without_a_key_is_an_error() {
    let dir = std::env::temp_dir().join(format!("s3_keyless_read_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let nvram = dir.join("space.nvram");
    let secret = b"TOP SECRET PAYLOAD".to_vec();

    std::env::set_var("SPACE_MASTER_KEY", "07".repeat(32));
    let keyed = router(&nvram, &registry);
    std::env::remove_var("SPACE_MASTER_KEY");
    let put = Request::put("/sec/secret.txt")
        .body(Body::from(secret.clone()))
        .unwrap();
    assert_eq!(send(&keyed, put).await.0, StatusCode::OK);
    drop(keyed);

    // The same store, served without the key.
    let keyless = router(&nvram, &registry);
    for range in [None, Some("bytes=4-9")] {
        let mut get = Request::get("/sec/secret.txt");
        if let Some(range) = range {
            get = get.header(header::RANGE, range);
        }
        let (status, body) = send(&keyless, get.body(Body::empty()).unwrap()).await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{range:?}: {text}"
        );
        assert!(text.contains("<Code>InternalError</Code>"), "{text}");
        assert!(text.contains("SPACE_MASTER_KEY"), "{text}");
    }

    // The key still reads it.
    std::env::set_var("SPACE_MASTER_KEY", "07".repeat(32));
    drop(keyless);
    let keyed = router(&nvram, &registry);
    std::env::remove_var("SPACE_MASTER_KEY");
    let get = Request::get("/sec/secret.txt").body(Body::empty()).unwrap();
    assert_eq!(send(&keyed, get).await, (StatusCode::OK, secret));

    drop(keyed);
    let _ = std::fs::remove_dir_all(&dir);
}

//! An encrypted object on a server without `SPACE_MASTER_KEY` is an error
//! response, not its ciphertext with a 200 (#155), and a PUT there is refused
//! rather than stored unencrypted (#157).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use capsule_registry::modular_pipeline::{registry_pipeline_from_log, StoreSecurity};
use capsule_registry::CapsuleRegistry;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use protocol_s3::{server, S3View};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

fn router(nvram: &Path, registry: &CapsuleRegistry, security: &StoreSecurity) -> Router {
    let log = NvramLog::open(nvram).unwrap();
    let handle = registry_pipeline_from_log(log, registry.clone(), security).unwrap();
    server::router(Arc::new(S3View::new_modular(handle)))
}

fn key() -> StoreSecurity {
    StoreSecurity::with_key(KeyManager::new([7; MASTER_KEY_SIZE]))
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

    let keyed = router(&nvram, &registry, &key());
    let create = Request::put("/sec").body(Body::empty()).unwrap();
    assert_eq!(send(&keyed, create).await.0, StatusCode::OK);
    let put = Request::put("/sec/secret.txt")
        .body(Body::from(secret.clone()))
        .unwrap();
    assert_eq!(send(&keyed, put).await.0, StatusCode::OK);
    drop(keyed);

    // The same store, served without the key. It refuses writes, rather
    // than storing them unencrypted (#157).
    let keyless = router(&nvram, &registry, &StoreSecurity::none());
    let put = Request::put("/sec/plain.txt")
        .body(Body::from("not secret"))
        .unwrap();
    let (status, body) = send(&keyless, put).await;
    let text = String::from_utf8_lossy(&body);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert!(text.contains("set SPACE_MASTER_KEY"), "{text}");
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
    drop(keyless);
    let keyed = router(&nvram, &registry, &key());
    let get = Request::get("/sec/secret.txt").body(Body::empty()).unwrap();
    assert_eq!(send(&keyed, get).await, (StatusCode::OK, secret));

    drop(keyed);
    let _ = std::fs::remove_dir_all(&dir);
}

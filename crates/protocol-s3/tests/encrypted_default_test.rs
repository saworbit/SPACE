//! S3 PUT encrypts by default when `SPACE_MASTER_KEY` is set (#82).
//!
//! This binary holds a single test because it changes a process-wide
//! environment variable.

use bytes::Bytes;
use capsule_registry::modular_pipeline::registry_pipeline_from_env;
use capsule_registry::CapsuleRegistry;
use futures::stream;
use protocol_s3::S3View;

#[tokio::test]
async fn put_encrypts_when_a_master_key_is_set() {
    let dir = std::env::temp_dir().join(format!("s3_encrypted_default_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let nvram = dir.join("space.nvram");

    std::env::set_var("SPACE_MASTER_KEY", "07".repeat(32));
    let handle = registry_pipeline_from_env(&nvram, registry.clone());
    std::env::remove_var("SPACE_MASTER_KEY");
    let s3 = S3View::new_modular(handle.unwrap());

    let secret = b"TOP-SECRET-MARKER\n".to_vec();
    let body = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(secret.clone()))]);
    let id = s3.put_object("bucket", "secret.txt", body).await.unwrap();

    assert!(registry.lookup(id).unwrap().policy.encryption.is_enabled());
    let stored = std::fs::read(&nvram).unwrap();
    assert!(
        !stored.windows(secret.len()).any(|w| w == secret.as_slice()),
        "plaintext found in the log"
    );

    drop(s3);
    let _ = std::fs::remove_dir_all(&dir);
}

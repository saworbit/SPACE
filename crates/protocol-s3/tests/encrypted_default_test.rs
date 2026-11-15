//! S3 PUT encrypts by default when the store is opened with a key (#82).

use bytes::Bytes;
use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use futures::stream;
use protocol_s3::S3View;
use test_support::TempDir;

#[tokio::test]
async fn put_encrypts_when_a_master_key_is_set() {
    let dir = TempDir::new("s3_encrypted_default");
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let nvram = dir.join("space.nvram");

    let key = StoreSecurity::with_key(KeyManager::new([7; MASTER_KEY_SIZE]));
    let log = nvram_sim::NvramLog::open(&nvram).unwrap();
    let s3 = S3View::new(registry.clone(), log, &key).unwrap();

    let secret = b"TOP-SECRET-MARKER\n".to_vec();
    let body = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(secret.clone()))]);
    s3.create_bucket("bucket").unwrap();
    let id = s3.put_object("bucket", "secret.txt", body).await.unwrap();

    assert!(registry.lookup(id).unwrap().policy.encryption.is_enabled());
    let stored = std::fs::read(&nvram).unwrap();
    assert!(
        !stored.windows(secret.len()).any(|w| w == secret.as_slice()),
        "plaintext found in the log"
    );

    drop(s3);
}

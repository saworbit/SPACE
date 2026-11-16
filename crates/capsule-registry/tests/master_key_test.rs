//! The key a registry pipeline opens with (#84), and plaintext writes to a
//! store that has a key (#157). Both are passed in as a `StoreSecurity`, so
//! nothing here touches the environment (#199). `spacectl` reads
//! `SPACE_MASTER_KEY` and `SPACE_ALLOW_PLAINTEXT`; its tests cover that.

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_log, RegistryPipelineHandle, StoreSecurity,
};
use capsule_registry::CapsuleRegistry;
use common::{EncryptionPolicy, Policy};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use test_support::TempDir;

#[tokio::test]
async fn the_store_key_decides_what_a_pipeline_may_write() {
    let dir = TempDir::new("space_master_key");
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let open = |security: StoreSecurity| {
        registry_pipeline_from_log(log.clone(), registry.clone(), &security)
    };
    let key = |byte: u8| StoreSecurity::with_key(KeyManager::new([byte; MASTER_KEY_SIZE]));

    // No key: the plain pipeline, which refuses encrypted writes.
    let mut handle = open(StoreSecurity::none()).unwrap();
    assert!(matches!(handle, RegistryPipelineHandle::Plain(_)));
    let encrypted = Policy {
        encryption: EncryptionPolicy::XtsAes256 { key_version: None },
        ..Policy::default()
    };
    assert!(handle
        .write_capsule(b"BRAVO-SECRET-PAYLOAD", &encrypted)
        .await
        .is_err());
    drop(handle);

    // A key: the encrypted pipeline.
    let handle = open(key(7)).unwrap();
    assert!(matches!(handle, RegistryPipelineHandle::Encrypted(_)));
    drop(handle);

    // The store now has a key, so without one it refuses writes (#157)...
    let mut handle = open(StoreSecurity::none()).unwrap();
    assert!(matches!(handle, RegistryPipelineHandle::Plain(_)));
    let err = handle
        .write_capsule(b"plain", &Policy::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("set SPACE_MASTER_KEY"), "{err}");
    drop(handle);

    // ...unless plaintext writes are asked for.
    let mut handle = open(StoreSecurity::plaintext()).unwrap();
    assert!(matches!(handle, RegistryPipelineHandle::Plain(_)));
    handle
        .write_capsule(b"plain", &Policy::default())
        .await
        .unwrap();
    drop(handle);

    // Another key is refused, with or without plaintext writes.
    for allow_plaintext in [false, true] {
        let security = StoreSecurity {
            allow_plaintext,
            ..key(8)
        };
        let err = open(security)
            .err()
            .expect("opened with another key")
            .to_string();
        assert!(err.contains("isn't this store's key"), "{err}");
    }

    drop((registry, log));
}

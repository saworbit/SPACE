//! `SPACE_MASTER_KEY` handling when building the registry pipeline (#84),
//! and `SPACE_ALLOW_PLAINTEXT` for a store that has a key (#157).
//!
//! This binary holds a single test because it changes a process-wide
//! environment variable.

use capsule_registry::modular_pipeline::{registry_pipeline_from_env, RegistryPipelineHandle};
use capsule_registry::CapsuleRegistry;
use common::{EncryptionPolicy, Policy};

#[tokio::test]
async fn master_key_must_be_valid_when_set() {
    let dir = std::env::temp_dir().join(format!("space_master_key_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let nvram = dir.join("space.nvram");

    // A key that is set but invalid is an error, not a quiet plain pipeline.
    for bad in ["not-hex", "0707"] {
        std::env::set_var("SPACE_MASTER_KEY", bad);
        assert!(
            registry_pipeline_from_env(&nvram, registry.clone()).is_err(),
            "SPACE_MASTER_KEY={bad} should be rejected"
        );
    }

    // No key: the plain pipeline, which refuses encrypted writes.
    std::env::remove_var("SPACE_MASTER_KEY");
    let mut handle = registry_pipeline_from_env(&nvram, registry.clone()).unwrap();
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

    // A valid key: the encrypted pipeline.
    std::env::set_var("SPACE_MASTER_KEY", "07".repeat(32));
    let handle = registry_pipeline_from_env(&nvram, registry.clone()).unwrap();
    std::env::remove_var("SPACE_MASTER_KEY");
    assert!(matches!(handle, RegistryPipelineHandle::Encrypted(_)));
    drop(handle);

    // The store now has a key, so without one it refuses writes (#157)...
    let mut handle = registry_pipeline_from_env(&nvram, registry.clone()).unwrap();
    assert!(matches!(handle, RegistryPipelineHandle::Plain(_)));
    let err = handle
        .write_capsule(b"plain", &Policy::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("set SPACE_MASTER_KEY"), "{err}");
    drop(handle);

    // ...unless plaintext writes are asked for, in exactly those words.
    std::env::set_var("SPACE_ALLOW_PLAINTEXT", "yes");
    let err = registry_pipeline_from_env(&nvram, registry.clone())
        .err()
        .expect("SPACE_ALLOW_PLAINTEXT=yes was taken")
        .to_string();
    assert!(err.contains("SPACE_ALLOW_PLAINTEXT must be 1"), "{err}");
    std::env::set_var("SPACE_ALLOW_PLAINTEXT", "1");
    let mut handle = registry_pipeline_from_env(&nvram, registry.clone()).unwrap();
    std::env::remove_var("SPACE_ALLOW_PLAINTEXT");
    assert!(matches!(handle, RegistryPipelineHandle::Plain(_)));
    handle
        .write_capsule(b"plain", &Policy::default())
        .await
        .unwrap();
    drop(handle);

    // Another key is refused, with or without it.
    std::env::set_var("SPACE_MASTER_KEY", "08".repeat(32));
    std::env::set_var("SPACE_ALLOW_PLAINTEXT", "1");
    let refused = registry_pipeline_from_env(&nvram, registry).err();
    std::env::remove_var("SPACE_MASTER_KEY");
    std::env::remove_var("SPACE_ALLOW_PLAINTEXT");
    let err = refused.expect("opened with another key").to_string();
    assert!(err.contains("isn't this store's key"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

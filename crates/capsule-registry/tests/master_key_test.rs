//! `SPACE_MASTER_KEY` handling when building the registry pipeline (#84).
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
    let handle = registry_pipeline_from_env(&nvram, registry).unwrap();
    std::env::remove_var("SPACE_MASTER_KEY");
    assert!(matches!(handle, RegistryPipelineHandle::Encrypted(_)));

    drop(handle);
    let _ = std::fs::remove_dir_all(&dir);
}

//! A store remembers that it's encrypted, and with which key (#157). Its
//! first open with a key records the key's fingerprint in the journal. From
//! then on another key is refused, and without one it refuses writes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use capsule_registry::modular_pipeline::registry_pipeline_from_log_with_key;
use capsule_registry::CapsuleRegistry;
use common::format::{CapsuleManifest, JournalRecord};
use common::{CapsuleId, EncryptionPolicy, Policy};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use uuid::Uuid;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("space_store_key_test")
        .join(format!("{name}_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(byte: u8) -> Arc<Mutex<KeyManager>> {
    Arc::new(Mutex::new(KeyManager::new([byte; MASTER_KEY_SIZE])))
}

fn fingerprint(byte: u8) -> String {
    let fingerprint = KeyManager::new([byte; MASTER_KEY_SIZE]).fingerprint();
    fingerprint[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn open_with(dir: &Path, registry: &CapsuleRegistry, byte: u8) -> anyhow::Result<()> {
    let log = NvramLog::open(dir.join("space.nvram"))?;
    registry_pipeline_from_log_with_key(log, registry.clone(), key(byte)).map(|_| ())
}

fn encrypted() -> Policy {
    Policy {
        encryption: EncryptionPolicy::XtsAes256 { key_version: None },
        ..Policy::default()
    }
}

#[tokio::test]
async fn another_key_is_refused_and_no_key_refuses_writes() {
    let dir = scratch("one_key");
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    assert_eq!(registry.store_key(), None);

    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let mut handle = registry_pipeline_from_log_with_key(log, registry.clone(), key(7)).unwrap();
    let recorded = registry
        .store_key()
        .expect("the first keyed open records the key");
    assert_eq!(recorded.fingerprint_hex(), fingerprint(7));
    assert_eq!(recorded.key_version, 1);
    assert_eq!(handle.store_key(), Some(recorded));
    let secret = handle
        .write_capsule(b"TOP SECRET PAYLOAD", &encrypted())
        .await
        .unwrap();
    drop(handle);

    // The same key opens it, and records nothing more.
    let journal = dir.join("space.journal");
    let len = std::fs::metadata(&journal).unwrap().len();
    open_with(&dir, &registry, 7).unwrap();
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), len);

    // Another key is refused, naming both fingerprints.
    let err = open_with(&dir, &registry, 8).unwrap_err().to_string();
    assert!(
        err.contains("SPACE_MASTER_KEY isn't this store's key"),
        "{err}"
    );
    assert!(err.contains(&fingerprint(7)), "{err}");
    assert!(err.contains(&fingerprint(8)), "{err}");
    assert_eq!(registry.store_key(), Some(recorded));

    // Without a key, it opens, but refuses writes: no segment, and no
    // capsule of segments found by dedup. Deletes still work.
    let mut keyless = registry.clone();
    keyless.check_store_key(None, false).unwrap();
    let err = keyless.check_writable().unwrap_err().to_string();
    assert!(err.contains("set SPACE_MASTER_KEY"), "{err}");
    assert!(err.contains(&fingerprint(7)), "{err}");
    assert!(err.contains("SPACE_ALLOW_PLAINTEXT=1"), "{err}");
    let err = keyless.alloc_segment().unwrap_err().to_string();
    assert!(err.contains("set SPACE_MASTER_KEY"), "{err}");
    let manifest = CapsuleManifest {
        capsule_id: CapsuleId::new(),
        created_at: 1_700_000_000,
        size: 0,
        digest: [0; 32],
        dedupe: true,
        requires_encryption: false,
        deduped_bytes: 0,
        policy: Policy::default(),
        entries: Vec::new(),
    };
    let err = keyless
        .commit(vec![JournalRecord::Capsule(Box::new(manifest))])
        .unwrap_err()
        .to_string();
    assert!(err.contains("set SPACE_MASTER_KEY"), "{err}");
    registry.check_writable().unwrap();
    let dropped = keyless.commit(vec![JournalRecord::Drop(secret)]).unwrap();
    assert_eq!(dropped.len(), 1);

    // Asked for, plaintext writes are allowed. A later check with the key
    // lifts the refusal too.
    keyless.check_store_key(None, true).unwrap();
    keyless.check_writable().unwrap();
    keyless.check_store_key(None, false).unwrap();
    assert!(keyless.check_writable().is_err());
    keyless.check_store_key(Some(recorded), false).unwrap();
    keyless.check_writable().unwrap();
    assert_eq!(registry.store_key(), Some(recorded));

    // The record outlives the process, journal compaction and the index.
    assert!(registry.compact_journal().unwrap().is_some());
    assert_eq!(registry.store_key(), Some(recorded));
    drop(registry);
    std::fs::remove_dir_all(dir.join("space.db")).unwrap();
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    assert_eq!(registry.store_key(), Some(recorded));
    assert!(open_with(&dir, &registry, 8).is_err());
    open_with(&dir, &registry, 7).unwrap();

    drop(registry);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_never_opened_with_a_key_records_nothing() {
    let dir = scratch("no_key");
    let mut registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    registry.check_store_key(None, false).unwrap();
    registry.check_writable().unwrap();
    assert_eq!(registry.store_key(), None);
    assert!(registry.journal_is_empty());

    // The store's key is recorded only by the check.
    let key = KeyManager::new([7; MASTER_KEY_SIZE]);
    let record = JournalRecord::Key(common::format::StoreKey {
        key_version: 1,
        fingerprint: key.fingerprint(),
    });
    assert!(registry.commit(vec![record]).is_err());
    assert_eq!(registry.store_key(), None);

    drop(registry);
    let _ = std::fs::remove_dir_all(&dir);
}

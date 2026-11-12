//! The data log through the registry pipeline: no sidecar to lose (#96), ref
//! counts recounted from capsules when a pipeline opens, and appends cut
//! short by a crash dropped on reopen.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_log_with_key, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::Policy;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use uuid::Uuid;

struct Store {
    dir: PathBuf,
    registry: Option<CapsuleRegistry>,
    key: Arc<Mutex<KeyManager>>,
}

impl Store {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("space_data_log_test")
            .join(format!("{name}_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        Self {
            dir,
            registry: Some(registry),
            key: Arc::new(Mutex::new(KeyManager::new([0x5Au8; MASTER_KEY_SIZE]))),
        }
    }

    fn registry(&self) -> &CapsuleRegistry {
        self.registry.as_ref().unwrap()
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("space.nvram")
    }

    fn log_len(&self) -> u64 {
        fs::metadata(self.log_path()).unwrap().len()
    }

    /// Open the log and a pipeline on it, as `spacectl` does on start.
    fn open(&self) -> (NvramLog, RegistryPipelineHandle) {
        let log = NvramLog::open(self.log_path()).unwrap();
        let handle = registry_pipeline_from_log_with_key(
            log.clone(),
            self.registry().clone(),
            Arc::clone(&self.key),
        )
        .unwrap();
        (log, handle)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // sled holds its files open until the registry is dropped.
        drop(self.registry.take());
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn patterned(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i % 251) as u8 ^ (i / 4093) as u8)
        .collect()
}

#[tokio::test]
async fn capsules_read_back_after_reopen_with_no_sidecar() {
    let store = Store::new("reopen");
    let plain = patterned(5 * 1024 * 1024 + 17);
    let secret = b"top secret ".repeat(1000);

    let (a, b) = {
        let (_log, mut handle) = store.open();
        let a = handle
            .write_capsule(&plain, &Policy::default())
            .await
            .unwrap();
        let policy = handle.default_policy();
        assert!(policy.encryption.is_enabled());
        let b = handle.write_capsule(&secret, &policy).await.unwrap();
        (a, b)
    };

    let mut sidecar = store.log_path().into_os_string();
    sidecar.push(".segments");
    assert!(!PathBuf::from(sidecar).exists());

    let (_log, handle) = store.open();
    assert_eq!(handle.read_capsule(a).await.unwrap(), plain);
    assert_eq!(handle.read_capsule(b).await.unwrap(), secret);
}

#[tokio::test]
async fn an_interrupted_dedup_write_leaves_no_extra_reference() {
    let store = Store::new("dedup_crash");
    let data = b"shared payload ".repeat(100);
    let policy = Policy::default();

    let (a, b, seg) = {
        let (log, mut handle) = store.open();
        let a = handle.write_capsule(&data, &policy).await.unwrap();
        let b = handle.write_capsule(&data, &policy).await.unwrap();
        let seg = store.registry().lookup(a).unwrap().segments[0];
        assert_eq!(store.registry().lookup(b).unwrap().segments[0], seg);

        // A third write dedups against them, then the process dies before
        // the write finishes.
        let mut write = handle.begin_write(&policy).unwrap();
        handle.write_chunk(&mut write, &data).await.unwrap();
        assert_eq!(log.get_segment_metadata(seg).unwrap().ref_count, 3);
        drop(write);
        (a, b, seg)
    };

    // Before format 1 the sidecar kept that third reference forever (#97).
    let (log, mut handle) = store.open();
    assert_eq!(log.get_segment_metadata(seg).unwrap().ref_count, 2);

    handle.delete_capsule(a).await.unwrap();
    assert_eq!(handle.read_capsule(b).await.unwrap(), data);
    handle.delete_capsule(b).await.unwrap();
    assert!(
        log.get_segment_metadata(seg).is_err(),
        "deleting the last capsule should free the segment"
    );
}

#[tokio::test]
async fn an_append_cut_short_by_a_crash_is_dropped_on_reopen() {
    let store = Store::new("torn");
    let data = patterned(300_000);

    let a = {
        let (_log, mut handle) = store.open();
        handle
            .write_capsule(&data, &Policy::default())
            .await
            .unwrap()
    };
    let clean_len = store.log_len();

    // The start of a record whose append the crash interrupted.
    let mut file = OpenOptions::new()
        .append(true)
        .open(store.log_path())
        .unwrap();
    file.write_all(b"SPCR\x01\x01\0\0\x10\0\0\0").unwrap();
    file.write_all(&[0xEE; 500]).unwrap();
    drop(file);

    let (log, handle) = store.open();
    assert!(log.damage().is_empty());
    assert_eq!(store.log_len(), clean_len);
    assert_eq!(handle.read_capsule(a).await.unwrap(), data);
}

#[tokio::test]
async fn orphans_from_an_interrupted_write_are_reclaimed_by_compaction() {
    let store = Store::new("orphans");
    let keep = patterned(200_000);
    let lost = b"never finished ".repeat(5000);

    let a = {
        let (_log, mut handle) = store.open();
        let a = handle
            .write_capsule(&keep, &Policy::default())
            .await
            .unwrap();
        let mut write = handle.begin_write(&Policy::default()).unwrap();
        handle.write_chunk(&mut write, &lost).await.unwrap();
        drop(write);
        a
    };
    let before = store.log_len();

    let (log, handle) = store.open();
    let reclaimed = log.compact().unwrap();
    assert!(reclaimed > 0);
    assert_eq!(store.log_len(), before - reclaimed);
    assert_eq!(handle.read_capsule(a).await.unwrap(), keep);
}

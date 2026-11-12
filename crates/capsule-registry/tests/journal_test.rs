//! The capsule journal through the registry: the sled index is rebuilt or
//! caught up from it on open, crashed writes leave no index entries, and a
//! journal and data log from different stores don't pair.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_log_with_key, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::{ContentHash, Policy, SegmentId};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use uuid::Uuid;

struct Store {
    dir: PathBuf,
    key: Arc<Mutex<KeyManager>>,
}

impl Store {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("space_journal_test")
            .join(format!("{name}_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            key: Arc::new(Mutex::new(KeyManager::new([0x3Cu8; MASTER_KEY_SIZE]))),
        }
    }

    fn db_path(&self) -> PathBuf {
        self.dir.join("space.db")
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.join("space.journal")
    }

    fn open(&self) -> (CapsuleRegistry, NvramLog, RegistryPipelineHandle) {
        let registry = CapsuleRegistry::open(self.db_path()).unwrap();
        let log = NvramLog::open(self.dir.join("space.nvram")).unwrap();
        let handle = registry_pipeline_from_log_with_key(
            log.clone(),
            registry.clone(),
            Arc::clone(&self.key),
        )
        .unwrap();
        (registry, log, handle)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A capsule record's size, segments, digest and dedup savings.
type CapsuleSummary = (u64, Vec<SegmentId>, Option<[u8; 32]>, u64);

/// Everything the index holds, in a comparable form.
#[derive(Debug, PartialEq)]
struct IndexState {
    capsules: BTreeMap<Uuid, CapsuleSummary>,
    refs: BTreeMap<SegmentId, u32>,
    content: BTreeMap<String, SegmentId>,
}

fn index_state(registry: &CapsuleRegistry) -> IndexState {
    IndexState {
        capsules: registry
            .capsules()
            .into_iter()
            .map(|c| {
                (
                    *c.id.as_uuid(),
                    (c.size, c.segments, c.digest, c.deduped_bytes),
                )
            })
            .collect(),
        refs: registry.segment_refs().unwrap().into_iter().collect(),
        content: registry
            .content_entries()
            .into_iter()
            .map(|(hash, seg)| (hash.as_str().to_string(), seg))
            .collect(),
    }
}

fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i % 251) as u8 ^ seed ^ (i / 3001) as u8)
        .collect()
}

#[tokio::test]
async fn losing_the_index_rebuilds_it_from_the_journal() {
    let store = Store::new("rebuild");
    let shared = patterned(300_000, 1);
    let secret = patterned(70_000, 2);

    let (before, a, c) = {
        let (registry, _log, mut handle) = store.open();
        let plain = Policy::default();
        let a = handle.write_capsule(&shared, &plain).await.unwrap();
        let _b = handle.write_capsule(&shared, &plain).await.unwrap();
        let encrypted = handle.default_policy();
        let c = handle.write_capsule(&secret, &encrypted).await.unwrap();
        let d = handle
            .write_capsule(&patterned(1000, 3), &plain)
            .await
            .unwrap();
        handle.delete_capsule(d).await.unwrap();
        (index_state(&registry), a, c)
    };
    assert_eq!(before.capsules.len(), 3);
    assert!(!before.content.is_empty());

    fs::remove_dir_all(store.db_path()).unwrap();

    let (registry, _log, handle) = store.open();
    assert_eq!(index_state(&registry), before);
    assert_eq!(handle.read_capsule(a).await.unwrap(), shared);
    assert_eq!(handle.read_capsule(c).await.unwrap(), secret);
}

#[tokio::test]
async fn records_the_index_missed_are_replayed_on_open() {
    let store = Store::new("replay");
    let first = patterned(50_000, 4);
    let second = patterned(60_000, 5);

    let a = {
        let (_registry, _log, mut handle) = store.open();
        handle
            .write_capsule(&first, &Policy::default())
            .await
            .unwrap()
    };
    // The index as it stood after the first write.
    let snapshot = store.dir.join("space.db.snapshot");
    copy_dir(&store.db_path(), &snapshot);

    let (b, after) = {
        let (registry, _log, mut handle) = store.open();
        let b = handle
            .write_capsule(&second, &Policy::default())
            .await
            .unwrap();
        handle.delete_capsule(a).await.unwrap();
        (b, index_state(&registry))
    };

    // A crash after the journal records were fsynced but before the index
    // took them: put the older index back.
    fs::remove_dir_all(store.db_path()).unwrap();
    copy_dir(&snapshot, &store.db_path());

    let (registry, _log, handle) = store.open();
    assert_eq!(index_state(&registry), after);
    assert!(registry.get_capsule(a).unwrap().is_none());
    assert_eq!(handle.read_capsule(b).await.unwrap(), second);
}

#[tokio::test]
async fn a_torn_journal_tail_is_dropped() {
    let store = Store::new("torn");
    let data = patterned(20_000, 6);
    let a = {
        let (_registry, _log, mut handle) = store.open();
        handle
            .write_capsule(&data, &Policy::default())
            .await
            .unwrap()
    };
    let clean_len = fs::metadata(store.journal_path()).unwrap().len();

    let mut file = OpenOptions::new()
        .append(true)
        .open(store.journal_path())
        .unwrap();
    file.write_all(b"SPCR\x10\x01\0\0\x40\0\0\0").unwrap();
    file.write_all(&[0x77; 40]).unwrap();
    drop(file);

    let (_registry, _log, handle) = store.open();
    assert_eq!(fs::metadata(store.journal_path()).unwrap().len(), clean_len);
    assert_eq!(handle.read_capsule(a).await.unwrap(), data);
}

#[tokio::test]
async fn a_crashed_write_leaves_no_entry_in_the_content_index() {
    let store = Store::new("pending");
    let data = patterned(40_000, 7);
    let policy = Policy::default();

    let hash: ContentHash = {
        let (registry, log, mut handle) = store.open();
        let mut write = handle.begin_write(&policy).unwrap();
        handle.write_chunk(&mut write, &data).await.unwrap();
        let seg = log.max_segment_id().unwrap();
        let hash = log.get_segment_metadata(seg).unwrap().content_hash.unwrap();
        // Other writes can dedup against it while the write is open.
        assert_eq!(registry.lookup_content(&hash), Some(seg));
        drop(write);
        hash
    };

    // After the crash, nothing in the index points at the orphan, so
    // compaction can remove it and a later write stores its own copy.
    let (registry, log, mut handle) = store.open();
    assert_eq!(registry.lookup_content(&hash), None);
    assert!(log.compact().unwrap() > 0);
    let id = handle.write_capsule(&data, &policy).await.unwrap();
    log.compact().unwrap();
    assert_eq!(handle.read_capsule(id).await.unwrap(), data);
}

#[tokio::test]
async fn a_journal_and_a_data_log_from_different_stores_are_refused() {
    let store = Store::new("mismatch");
    {
        let (_registry, _log, mut handle) = store.open();
        handle
            .write_capsule(b"belongs to this store", &Policy::default())
            .await
            .unwrap();
    }
    let other = Store::new("mismatch_other");
    let registry = CapsuleRegistry::open(store.db_path()).unwrap();
    let log = NvramLog::open(other.dir.join("space.nvram")).unwrap();
    let err = registry_pipeline_from_log_with_key(log, registry, Arc::clone(&store.key))
        .err()
        .unwrap();
    assert!(err.to_string().contains("different stores"), "{err}");
}

#[tokio::test]
async fn a_new_journal_takes_the_data_logs_store_id() {
    let store = Store::new("adopt");
    let (registry, log, _handle) = store.open();
    assert_eq!(registry.store_id(), log.store_id());
    drop((registry, log, _handle));
    let (registry, log, _handle) = store.open();
    assert_eq!(registry.store_id(), log.store_id());
}

#[tokio::test]
async fn the_digest_is_blake3_of_the_capsule() {
    let store = Store::new("digest");
    let data = patterned(5 * 1024 * 1024 + 9, 8);
    let (registry, _log, mut handle) = store.open();
    let id = handle
        .write_capsule(&data, &Policy::default())
        .await
        .unwrap();
    assert_eq!(
        registry.lookup(id).unwrap().digest,
        Some(*blake3::hash(&data).as_bytes())
    );
    let empty = handle.write_capsule(b"", &Policy::default()).await.unwrap();
    assert_eq!(
        registry.lookup(empty).unwrap().digest,
        Some(*blake3::hash(b"").as_bytes())
    );
}

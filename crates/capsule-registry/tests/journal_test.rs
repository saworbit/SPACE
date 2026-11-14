//! The capsule journal through the registry: the sled index, names included,
//! is rebuilt or caught up from it on open, a commit is all or nothing,
//! crashed writes leave no index entries, and a journal and data log from
//! different stores don't pair.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_log_with_key, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::format::JournalRecord;
use common::{CapsuleId, ContentHash, Policy, SegmentId};
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

/// Every name in `ns` and the capsule it points at.
fn names(registry: &CapsuleRegistry, ns: &str) -> Vec<(String, CapsuleId)> {
    registry
        .list_names(ns, "", None, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|(name, binding)| (name, binding.capsule_id))
        .collect()
}

fn bind(ns: &str, name: &str, capsule_id: CapsuleId) -> JournalRecord {
    JournalRecord::Bind {
        ns: ns.into(),
        name: name.into(),
        capsule_id,
        metadata: Vec::new(),
    }
}

fn journal_len(store: &Store) -> u64 {
    fs::metadata(store.journal_path()).unwrap().len()
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

#[tokio::test]
async fn names_are_rebuilt_from_the_journal() {
    let store = Store::new("names");
    let ns = "s3:demo";
    let (a, before, created_at) = {
        let (registry, _log, mut handle) = store.open();
        let policy = Policy::default();
        let a = handle.write_capsule(b"alpha", &policy).await.unwrap();
        let b = handle.write_capsule(b"bravo", &policy).await.unwrap();
        registry.create_namespace(ns).unwrap();
        registry.create_namespace("s3:empty").unwrap();
        handle
            .commit(vec![bind(ns, "a.txt", a), bind(ns, "b.txt", b)])
            .unwrap();
        // Rebinding replaces; unbinding a name that isn't there does nothing.
        handle
            .commit(vec![
                bind(ns, "b.txt", a),
                JournalRecord::Unbind {
                    ns: ns.into(),
                    name: "missing".into(),
                },
            ])
            .unwrap();
        let dropped = handle
            .commit(vec![
                JournalRecord::Unbind {
                    ns: ns.into(),
                    name: "b.txt".into(),
                },
                JournalRecord::Drop(b),
            ])
            .unwrap();
        assert_eq!(dropped.len(), 1);
        handle.release(&dropped[0]).await.unwrap();
        (
            a,
            index_state(&registry),
            registry.namespace_created_at(ns).unwrap(),
        )
    };
    assert!(created_at.is_some());

    fs::remove_dir_all(store.db_path()).unwrap();

    let (registry, _log, handle) = store.open();
    assert_eq!(index_state(&registry), before);
    assert_eq!(names(&registry, ns), [("a.txt".to_string(), a)]);
    assert_eq!(registry.namespace_created_at(ns).unwrap(), created_at);
    assert!(registry.namespace_created_at("s3:empty").unwrap().is_some());
    assert!(registry.namespace_created_at("s3:never").unwrap().is_none());
    assert_eq!(handle.read_capsule(a).await.unwrap(), b"alpha");
}

#[tokio::test]
async fn a_refused_commit_writes_nothing() {
    let store = Store::new("refused");
    let (registry, _log, mut handle) = store.open();
    let a = handle
        .write_capsule(b"kept", &Policy::default())
        .await
        .unwrap();
    let len = journal_len(&store);
    let missing = CapsuleId::new();

    let refused = [
        vec![bind("s3:b", "k", a), JournalRecord::Drop(missing)],
        vec![JournalRecord::Drop(a), JournalRecord::Drop(a)],
        vec![JournalRecord::Batch(vec![bind("s3:b", "k", a)])],
        vec![bind("", "k", a)],
    ];
    for records in refused {
        assert!(handle.commit(records.clone()).is_err(), "{records:?}");
        assert_eq!(journal_len(&store), len, "{records:?} reached the journal");
    }
    assert!(names(&registry, "s3:b").is_empty());
    assert!(registry.get_capsule(a).unwrap().is_some());
}

#[tokio::test]
async fn a_torn_batch_is_dropped_whole() {
    let store = Store::new("torn_batch");
    let ns = "s3:b";
    let policy = Policy::default();
    let old = patterned(30_000, 9);
    let (a, new_id) = {
        let (registry, _log, mut handle) = store.open();
        let a = handle.write_capsule(&old, &policy).await.unwrap();
        handle.commit(vec![bind(ns, "k", a)]).unwrap();
        let before = journal_len(&store);

        // An overwrite: the new capsule, its key and the drop of the old one.
        let mut write = handle.begin_write(&policy).unwrap();
        handle
            .write_chunk(&mut write, &patterned(20_000, 10))
            .await
            .unwrap();
        let new_id = write.capsule_id();
        let dropped = handle
            .finish_write_with(write, vec![bind(ns, "k", new_id), JournalRecord::Drop(a)])
            .await
            .unwrap();
        assert_eq!(dropped.len(), 1);
        assert_eq!(names(&registry, ns), [("k".to_string(), new_id)]);
        assert!(journal_len(&store) > before + 1);
        (a, new_id)
    };

    // Cut the batch's last byte, as a crash during its append would.
    let file = OpenOptions::new()
        .write(true)
        .open(store.journal_path())
        .unwrap();
    file.set_len(journal_len(&store) - 1).unwrap();
    drop(file);

    let (registry, _log, handle) = store.open();
    assert_eq!(names(&registry, ns), [("k".to_string(), a)]);
    assert!(registry.get_capsule(new_id).unwrap().is_none());
    assert_eq!(handle.read_capsule(a).await.unwrap(), old);
}

#[test]
fn s3_names_from_before_the_journal_are_refused_or_dropped() {
    // A space.db whose S3 buckets were never journaled is refused, so going
    // back to the build that wrote it still finds them.
    let store = Store::new("legacy");
    {
        let db = sled::open(store.db_path()).unwrap();
        db.open_tree("s3_buckets")
            .unwrap()
            .insert("demo", vec![0u8; 8])
            .unwrap();
        db.flush().unwrap();
    }
    let err = CapsuleRegistry::open(store.db_path()).err().unwrap();
    assert!(
        err.to_string().contains("before the capsule journal"),
        "{err}"
    );

    // An index being rebuilt from the journal drops them: nothing reads
    // them, and the journal doesn't hold them.
    let store = Store::new("legacy_rebuild");
    drop(CapsuleRegistry::open(store.db_path()).unwrap());
    {
        let db = sled::open(store.db_path()).unwrap();
        db.open_tree("s3_objects")
            .unwrap()
            .insert("demo\0k", vec![0u8; 24])
            .unwrap();
        db.flush().unwrap();
    }
    // Another store's journal doesn't match the index, so it's rebuilt.
    let other = Store::new("legacy_other");
    drop(CapsuleRegistry::open(other.db_path()).unwrap());
    fs::copy(other.journal_path(), store.journal_path()).unwrap();
    drop(CapsuleRegistry::open(store.db_path()).unwrap());
    let db = sled::open(store.db_path()).unwrap();
    assert!(!db
        .tree_names()
        .iter()
        .any(|name| name.as_ref() == b"s3_objects"));
}

#[tokio::test]
async fn segment_ids_are_not_reused_after_compaction_and_a_lost_index() {
    let store = Store::new("id_floor");
    let policy = Policy::default();
    let kept = patterned(10_000, 11);
    let (a, dropped_ids) = {
        let (registry, log, mut handle) = store.open();
        let a = handle.write_capsule(&kept, &policy).await.unwrap();
        let b = handle
            .write_capsule(&patterned(10_000, 12), &policy)
            .await
            .unwrap();
        let dropped_ids = registry.lookup(b).unwrap().segments;
        handle.delete_capsule(b).await.unwrap();
        // The highest ids leave the data log.
        assert!(log.compact().unwrap() > 0);
        (a, dropped_ids)
    };
    // The allocator's counter goes with space.db; the data log keeps the
    // floor in its FILE record.
    fs::remove_dir_all(store.db_path()).unwrap();

    let (registry, _log, mut handle) = store.open();
    let c = handle
        .write_capsule(&patterned(10_000, 13), &policy)
        .await
        .unwrap();
    let highest_dropped = dropped_ids.iter().max().unwrap();
    for seg in registry.lookup(c).unwrap().segments {
        assert!(seg > *highest_dropped, "segment id {seg:?} reused");
    }
    assert_eq!(handle.read_capsule(a).await.unwrap(), kept);
}

#[tokio::test]
async fn compaction_keeps_the_segments_of_an_open_write() {
    let store = Store::new("open_write");
    let policy = Policy::default();
    let data = patterned(50_000, 14);
    let (registry, log, mut handle) = store.open();
    let doomed = handle
        .write_capsule(&patterned(20_000, 15), &policy)
        .await
        .unwrap();
    handle.delete_capsule(doomed).await.unwrap();

    let mut write = handle.begin_write(&policy).unwrap();
    handle.write_chunk(&mut write, &data).await.unwrap();
    // The open write's segment is referenced only by the write itself.
    assert!(log.compact().unwrap() > 0);
    let id = handle.finish_write(write).await.unwrap();

    assert_eq!(handle.read_capsule(id).await.unwrap(), data);
    drop((registry, log, handle));
    let (_registry, log, handle) = store.open();
    assert_eq!(log.file_header().generation, 1);
    assert_eq!(handle.read_capsule(id).await.unwrap(), data);
}

/// A store whose capsule journal is missing doesn't open, and nothing in it
/// changes (#194). A new journal would rebuild the index as empty, and the
/// next reclaim would delete every segment.
#[tokio::test]
async fn a_store_missing_its_journal_is_refused() {
    let store = Store::new("missing_journal");
    {
        let (_registry, _log, mut handle) = store.open();
        handle
            .write_capsule(&patterned(300_000, 21), &Policy::default())
            .await
            .unwrap();
    }
    let nvram = store.dir.join("space.nvram");
    let log_bytes = fs::read(&nvram).unwrap();

    // space.db shows the store was used, so it isn't opened without its
    // journal, and no new journal is made.
    fs::remove_file(store.journal_path()).unwrap();
    let err = CapsuleRegistry::open(store.db_path()).err().unwrap();
    assert!(err.to_string().contains("is missing"), "{err}");
    assert!(!store.journal_path().exists());

    // With space.db gone too, a new journal can't take over a data log that
    // holds segments.
    fs::remove_dir_all(store.db_path()).unwrap();
    let registry = CapsuleRegistry::open(store.db_path()).unwrap();
    let log = NvramLog::open(&nvram).unwrap();
    let err = registry_pipeline_from_log_with_key(log, registry, Arc::clone(&store.key))
        .err()
        .unwrap();
    assert!(err.to_string().contains("is new"), "{err}");
    assert_eq!(fs::read(&nvram).unwrap(), log_bytes);
}

/// A capsule's names are indexed under it, so deleting it unbinds them in
/// the same batch (#138). A name rebound to another capsule moves with it,
/// and the index rebuilt from the journal agrees.
#[tokio::test]
async fn deleting_a_capsule_unbinds_the_names_that_point_at_it() {
    let store = Store::new("delete_names");
    let ns = "s3:demo";
    let (a, b) = {
        let (registry, _log, mut handle) = store.open();
        let policy = Policy::default();
        let a = handle.write_capsule(b"alpha", &policy).await.unwrap();
        let b = handle.write_capsule(b"bravo", &policy).await.unwrap();
        registry
            .commit(vec![
                bind(ns, "one", a),
                bind(ns, "two", a),
                bind("s3:other", "one", a),
                bind(ns, "moved", a),
            ])
            .unwrap();
        // Rebinding "moved" takes it off `a`, and so does unbinding
        // "back" before it's bound to `b`.
        registry.commit(vec![bind(ns, "moved", b)]).unwrap();
        registry.commit(vec![bind(ns, "back", a)]).unwrap();
        registry
            .commit(vec![JournalRecord::Unbind {
                ns: ns.into(),
                name: "back".into(),
            }])
            .unwrap();
        registry.commit(vec![bind(ns, "back", b)]).unwrap();
        let mut held = registry.names_of(a).unwrap();
        held.sort();
        assert_eq!(
            held,
            [
                (ns.to_string(), "one".to_string()),
                (ns.to_string(), "two".to_string()),
                ("s3:other".to_string(), "one".to_string()),
            ]
        );

        handle.delete_capsule(a).await.unwrap();
        assert!(registry.names_of(a).unwrap().is_empty());
        assert_eq!(
            names(&registry, ns),
            [("back".to_string(), b), ("moved".to_string(), b)]
        );
        assert!(names(&registry, "s3:other").is_empty());
        (a, b)
    };

    fs::remove_dir_all(store.db_path()).unwrap();
    let (registry, _log, _handle) = store.open();
    assert!(registry.names_of(a).unwrap().is_empty());
    assert_eq!(
        registry.names_of(b).unwrap(),
        [
            (ns.to_string(), "back".to_string()),
            (ns.to_string(), "moved".to_string())
        ]
    );
    assert_eq!(
        names(&registry, ns),
        [("back".to_string(), b), ("moved".to_string(), b)]
    );
}

/// An index rebuilt in place, here because an older copy of the journal was
/// restored, keeps no name under a capsule it no longer points at. So
/// deleting that capsule can't unbind a name another capsule holds (#138).
#[tokio::test]
async fn a_rebuilt_index_keeps_no_stale_name_under_a_capsule() {
    let store = Store::new("stale_names");
    let ns = "s3:demo";
    let backup = store.dir.join("journal.bak");
    let (a, b) = {
        let (registry, _log, mut handle) = store.open();
        let policy = Policy::default();
        let a = handle.write_capsule(b"alpha", &policy).await.unwrap();
        let b = handle.write_capsule(b"bravo", &policy).await.unwrap();
        registry.commit(vec![bind(ns, "x", a)]).unwrap();
        fs::copy(store.journal_path(), &backup).unwrap();
        registry.commit(vec![bind(ns, "x", b)]).unwrap();
        (a, b)
    };
    // The index is ahead of the restored journal, so it's rebuilt.
    fs::copy(&backup, store.journal_path()).unwrap();
    let (registry, _log, mut handle) = store.open();
    assert_eq!(names(&registry, ns), [("x".to_string(), a)]);
    handle.delete_capsule(b).await.unwrap();
    assert_eq!(names(&registry, ns), [("x".to_string(), a)]);
}

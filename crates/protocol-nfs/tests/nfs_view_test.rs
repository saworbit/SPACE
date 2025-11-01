use capsule_registry::CapsuleRegistry;
use nvram_sim::NvramLog;
use protocol_nfs::NfsView;
use std::fs;

fn teardown(prefix: &str) {
    let _ = fs::remove_file(format!("{}.nvram", prefix));
    let _ = fs::remove_file(format!("{}.nvram.segments", prefix));
    let _ = fs::remove_file(format!("{}.metadata", prefix));
    let _ = fs::remove_file(format!("{}.nfs.json", prefix));
}

fn setup(prefix: &str) -> NfsView {
    teardown(prefix);
    let log_path = format!("{}.nvram", prefix);
    let meta_path = format!("{}.metadata", prefix);
    let namespace_path = format!("{}.nfs.json", prefix);
    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    NfsView::open(registry, nvram, namespace_path).unwrap()
}

#[test]
fn nfs_basic_crud_flow() {
    let prefix = "test_nfs_basic";
    let nfs = setup(prefix);

    nfs.mkdir("/data/logs").unwrap();

    let first_capsule = nfs
        .write_file("/data/logs/app.log", b"hello, world".to_vec())
        .unwrap();

    let fetched = nfs.read_file("/data/logs/app.log").unwrap();
    assert_eq!(fetched, b"hello, world");

    let entries = nfs.list_directory("/data").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "logs");
    assert!(entries[0].is_directory());

    let log_dir = nfs.metadata("/data/logs").unwrap();
    assert!(log_dir.is_directory());

    let file_meta = nfs.metadata("/data/logs/app.log").unwrap();
    assert_eq!(file_meta.size(), 12);
    assert_eq!(file_meta.capsule_id().unwrap(), first_capsule);

    let sliced = nfs.read_range("/data/logs/app.log", 7, 5).unwrap();
    assert_eq!(sliced, b"world");

    let second_capsule = nfs
        .write_file("/data/logs/app.log", b"goodbye".to_vec())
        .unwrap();
    assert_ne!(first_capsule, second_capsule);

    let updated = nfs.read_file("/data/logs/app.log").unwrap();
    assert_eq!(updated, b"goodbye");

    nfs.delete("/data/logs/app.log").unwrap();
    assert!(nfs.read_file("/data/logs/app.log").is_err());

    // Directory still exists, but should now be empty.
    let log_entries = nfs.list_directory("/data/logs").unwrap();
    assert!(log_entries.is_empty());

    drop(nfs);
    teardown(prefix);
}

#[test]
fn nfs_relative_paths_are_normalised() {
    let prefix = "test_nfs_normalise";
    let nfs = setup(prefix);

    nfs.write_file("tmp/../tmp/file.txt", b"bytes".to_vec())
        .unwrap();
    let meta = nfs.metadata("/tmp/file.txt").unwrap();
    assert_eq!(meta.name(), "file.txt");
    assert_eq!(meta.size(), 5);

    drop(nfs);
    teardown(prefix);
}

#[test]
fn nfs_persists_namespace_state() {
    let prefix = "test_nfs_persist";
    teardown(prefix);
    let log_path = format!("{}.nvram", prefix);
    let meta_path = format!("{}.metadata", prefix);
    let namespace_path = format!("{}.nfs.json", prefix);

    {
        let registry = CapsuleRegistry::open(&meta_path).unwrap();
        let nvram = NvramLog::open(&log_path).unwrap();
        let nfs = NfsView::open(registry, nvram, &namespace_path).unwrap();
        nfs.mkdir("/persist").unwrap();
        nfs.write_file("/persist/file.txt", b"persisted".to_vec())
            .unwrap();
    }

    {
        let registry = CapsuleRegistry::open(&meta_path).unwrap();
        let nvram = NvramLog::open(&log_path).unwrap();
        let nfs = NfsView::open(registry, nvram, &namespace_path).unwrap();
        let meta = nfs.metadata("/persist/file.txt").unwrap();
        assert_eq!(meta.size(), 9);
        let data = nfs.read_file("/persist/file.txt").unwrap();
        assert_eq!(data, b"persisted");
    }

    teardown(prefix);
}

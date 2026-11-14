//! Storage roots in on-disk format 1 (#135, `docs/on-disk-format.md` §10):
//! one framed SEGMENT record per segment file, a store file with the store
//! id, record checksums that light scrub can check, and older layouts
//! refused.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::traits::{StorageBackend, StorageTransaction};
use common::{ContentHash, Segment, SegmentId};
use storage::TokioFsBackend;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("space_fs_root_test")
        .join(format!("{name}_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn segment_file(root: &Path, id: u64) -> PathBuf {
    root.join("segments").join(format!("{id}.seg"))
}

fn flip(path: &Path, at: usize) {
    let mut bytes = fs::read(path).unwrap();
    bytes[at] ^= 0x20;
    fs::write(path, bytes).unwrap();
}

fn described(id: u64, data: &[u8], encrypted: bool) -> Segment {
    Segment {
        id: SegmentId(id),
        len: data.len() as u32,
        plain_len: Some(data.len() as u32 + 7),
        compressed: true,
        compression_algo: "zstd:3".to_string(),
        content_hash: Some(ContentHash::from_bytes(&[0x42; 32])),
        ref_count: 1,
        encrypted,
        encryption_version: encrypted.then_some(1),
        key_version: encrypted.then_some(3),
        tweak_nonce: encrypted.then_some([0x11; 16]),
        integrity_tag: encrypted.then_some([0x22; 16]),
        ..Segment::default()
    }
}

async fn write(backend: &mut TokioFsBackend, id: u64, data: &[u8], meta: Segment) {
    let mut txn = backend.begin_txn().await.unwrap();
    txn.append(SegmentId(id), data).await.unwrap();
    txn.set_segment_metadata(SegmentId(id), meta).await.unwrap();
    txn.commit().await.unwrap();
}

#[tokio::test]
async fn a_segment_file_holds_its_descriptor_and_bytes() {
    let root = scratch("round_trip");
    let mut backend = TokioFsBackend::open(&root).await.unwrap();
    let data = b"stored bytes, compressed and encrypted".to_vec();
    let meta = described(7, &data, true);
    write(&mut backend, 7, &data, meta.clone()).await;

    assert!(root.join("store").is_file());
    assert!(segment_file(&root, 7).is_file());
    assert!(!root.join("metadata").exists());
    assert_eq!(backend.read(SegmentId(7)).await.unwrap(), data);
    assert_eq!(backend.segment_ids().await.unwrap(), vec![SegmentId(7)]);

    let got = backend.metadata(SegmentId(7)).await.unwrap();
    assert_eq!(got.len, data.len() as u32);
    for (what, ok) in [
        ("plain_len", got.plain_len == meta.plain_len),
        ("compressed", got.compressed),
        (
            "compression_algo",
            got.compression_algo == meta.compression_algo,
        ),
        ("content_hash", got.content_hash == meta.content_hash),
        ("encrypted", got.encrypted),
        (
            "encryption_version",
            got.encryption_version == meta.encryption_version,
        ),
        ("key_version", got.key_version == meta.key_version),
        ("tweak", got.tweak_nonce == meta.tweak_nonce),
        ("integrity_tag", got.integrity_tag == meta.integrity_tag),
        ("ref_count", got.ref_count == 1),
    ] {
        assert!(ok, "{what} didn't survive the round trip");
    }
    assert!(!fs::read_dir(root.join("segments")).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));

    drop(backend);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn verify_stored_catches_a_flipped_byte_in_any_segment_file() {
    let root = scratch("verify");
    let mut backend = TokioFsBackend::open(&root).await.unwrap();
    let data = vec![0x5A; 4096];
    write(&mut backend, 1, &data, described(1, &data, false)).await;
    write(&mut backend, 2, &data, described(2, &data, true)).await;
    write(&mut backend, 3, &data, described(3, &data, false)).await;
    for id in 1..=3 {
        assert_eq!(
            backend.verify_stored(SegmentId(id)).await.unwrap(),
            Some(true)
        );
    }

    // A stored byte, in a plain and an encrypted segment: the body checksum.
    let len = fs::metadata(segment_file(&root, 1)).unwrap().len() as usize;
    flip(&segment_file(&root, 1), len - 100);
    flip(&segment_file(&root, 2), len - 100);
    // A descriptor byte: the head checksum. Its metadata can't be read.
    flip(&segment_file(&root, 3), 60);

    for id in 1..=3 {
        assert_eq!(
            backend.verify_stored(SegmentId(id)).await.unwrap(),
            Some(false),
            "segment {id}"
        );
    }
    assert!(backend.metadata(SegmentId(3)).await.is_err());
    assert!(backend.read(SegmentId(3)).await.is_err());
    assert!(
        backend.verify_stored(SegmentId(9)).await.is_err(),
        "no such segment"
    );

    drop(backend);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_file_holding_another_segment_is_rejected() {
    let root = scratch("renamed");
    let mut backend = TokioFsBackend::open(&root).await.unwrap();
    write(&mut backend, 1, b"one", described(1, b"one", false)).await;
    fs::copy(segment_file(&root, 1), segment_file(&root, 5)).unwrap();

    let err = backend.read(SegmentId(5)).await.unwrap_err();
    assert!(err.to_string().contains("holds segment 1"), "{err:#}");
    assert_eq!(
        backend.verify_stored(SegmentId(5)).await.unwrap(),
        Some(false)
    );

    drop(backend);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn reopening_keeps_the_store_id_and_drops_unfinished_writes() {
    let root = scratch("reopen");
    let mut backend = TokioFsBackend::open(&root).await.unwrap();
    let store_id = backend.store_id();
    write(&mut backend, 4, b"kept", described(4, b"kept", false)).await;
    drop(backend);
    // A write that never got as far as its rename.
    fs::write(root.join("segments").join("8.seg.tmp"), b"half a record").unwrap();

    let backend = TokioFsBackend::open(&root).await.unwrap();
    assert_eq!(backend.store_id(), store_id);
    assert!(!root.join("segments").join("8.seg.tmp").exists());
    assert_eq!(backend.read(SegmentId(4)).await.unwrap(), b"kept");
    assert_eq!(backend.segment_id_floor().unwrap(), 5);

    drop(backend);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_root_in_the_old_layout_is_refused() {
    let root = scratch("old_layout");
    fs::create_dir_all(root.join("segments")).unwrap();
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::write(root.join("segments").join("1.bin"), b"bytes").unwrap();
    fs::write(root.join("metadata").join("1.json"), b"{}").unwrap();
    let err = TokioFsBackend::open(&root).await.err().unwrap();
    assert!(
        err.to_string().contains("before on-disk format 1"),
        "{err:#}"
    );
    assert!(!root.join("store").exists(), "nothing is written to it");

    // Segment files without the store file can't be paired with a journal.
    let orphaned = scratch("no_store");
    let mut backend = TokioFsBackend::open(&orphaned).await.unwrap();
    write(&mut backend, 1, b"one", described(1, b"one", false)).await;
    drop(backend);
    fs::remove_file(orphaned.join("store")).unwrap();
    let err = TokioFsBackend::open(&orphaned).await.err().unwrap();
    assert!(err.to_string().contains("no store file"), "{err:#}");

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&orphaned);
}

#[tokio::test]
async fn ref_counts_are_recounted_and_kept_in_memory() {
    let root = scratch("refs");
    let mut backend = TokioFsBackend::open(&root).await.unwrap();
    write(&mut backend, 1, b"one", described(1, b"one", false)).await;
    write(&mut backend, 2, b"two", described(2, b"two", false)).await;
    let on_disk = fs::read(segment_file(&root, 1)).unwrap();

    let report = backend
        .recount_references(&HashMap::from([(SegmentId(1), 3), (SegmentId(6), 1)]))
        .unwrap();
    assert_eq!(report.orphans, vec![SegmentId(2)]);
    assert_eq!(report.missing, vec![SegmentId(6)]);
    assert_eq!(backend.metadata(SegmentId(1)).await.unwrap().ref_count, 3);
    assert_eq!(backend.metadata(SegmentId(2)).await.unwrap().ref_count, 0);

    // A dedup hit or a release changes only the count; the file stays.
    let mut meta = backend.metadata(SegmentId(1)).await.unwrap();
    meta.ref_count = 2;
    let mut txn = backend.begin_txn().await.unwrap();
    txn.set_segment_metadata(SegmentId(1), meta.clone())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(backend.metadata(SegmentId(1)).await.unwrap().ref_count, 2);
    assert_eq!(fs::read(segment_file(&root, 1)).unwrap(), on_disk);

    let mut txn = backend.begin_txn().await.unwrap();
    txn.set_segment_metadata(SegmentId(9), meta).await.unwrap();
    assert!(txn.commit().await.is_err(), "no such segment");

    backend.delete(SegmentId(2)).await.unwrap();
    assert!(!segment_file(&root, 2).exists());
    assert_eq!(backend.segment_ids().await.unwrap(), vec![SegmentId(1)]);

    drop(backend);
    let _ = fs::remove_dir_all(&root);
}

/// The store file's FILE record is checked as the data log's is, and a bad
/// one is refused in the same words (#192).
#[tokio::test]
async fn the_store_file_is_checked_as_every_format_one_file_is() {
    use common::format::{encode_head, kind, FileHeader, FileKind, FILE_VERSION};

    let root_header = FileHeader::new(FileKind::StorageRoot, 0);
    let log_header = FileHeader::new(FileKind::DataLog, 0);
    let mut future_format = root_header.clone();
    future_format.format += 1;
    let mut log_future_format = log_header.clone();
    log_future_format.format += 1;
    let mut padded = root_header.encode_record();
    padded.extend_from_slice(b"more");
    let next = FILE_VERSION + 1;
    // (store file, data log, what both errors say)
    let cases: Vec<(Vec<u8>, Vec<u8>, String)> = vec![
        (
            encode_head(kind::FILE, next, 0, &root_header.encode(), &[]).unwrap(),
            encode_head(kind::FILE, next, 0, &log_header.encode(), &[]).unwrap(),
            format!("has a FILE record with layout version {next}, which this build can't read"),
        ),
        (
            log_header.encode_record(),
            FileHeader::new(FileKind::Journal, 0).encode_record(),
            "is a ".to_string(),
        ),
        (
            future_format.encode_record(),
            log_future_format.encode_record(),
            format!(
                "is store format {}; this build reads format",
                future_format.format
            ),
        ),
        (
            b"not a store file at all, just some bytes".to_vec(),
            b"not a data log at all, just some bytes".to_vec(),
            "isn't an on-disk format 1".to_string(),
        ),
    ];
    for (store, log, says) in cases {
        let root = scratch("store_file");
        fs::write(root.join("store"), &store).unwrap();
        let err = format!("{:#}", TokioFsBackend::open(&root).await.err().unwrap());
        assert!(err.contains(&says), "{err}");

        let log_path = root.join("space.nvram");
        fs::write(&log_path, &log).unwrap();
        let log_err = format!("{:#}", nvram_sim::NvramLog::open(&log_path).err().unwrap());
        assert!(log_err.contains(&says), "{log_err}");
        let _ = fs::remove_dir_all(&root);
    }

    // The wrong kind, in the same words.
    let root = scratch("store_file_kind");
    fs::write(root.join("store"), log_header.encode_record()).unwrap();
    let err = format!("{:#}", TokioFsBackend::open(&root).await.err().unwrap());
    assert!(
        err.contains("is a data log, not a storage root store file"),
        "{err}"
    );
    let log_path = root.join("space.nvram");
    fs::write(&log_path, root_header.encode_record()).unwrap();
    let log_err = format!("{:#}", nvram_sim::NvramLog::open(&log_path).err().unwrap());
    assert!(
        log_err.contains("is a storage root store file, not a data log"),
        "{log_err}"
    );

    // A store file is its FILE record alone.
    fs::write(root.join("store"), &padded).unwrap();
    let err = format!("{:#}", TokioFsBackend::open(&root).await.err().unwrap());
    assert!(
        err.contains("isn't an on-disk format 1 storage root store file (it holds more than its FILE record)"),
        "{err}"
    );
    let _ = fs::remove_dir_all(&root);
}

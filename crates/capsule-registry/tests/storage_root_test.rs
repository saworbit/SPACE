//! A registry over a storage root (#135, `docs/on-disk-format.md` §10):
//! light scrub catches damage in segment files, the capsule journal is
//! paired with the root's store id, and a lost `space.db` can't lead to a
//! new segment overwriting an old one.

use std::fs;
use std::path::{Path, PathBuf};

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_fs_backend, AutoFsBackend, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::scrub::{ScrubKind, ScrubResult};
use common::{Policy, SegmentId};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("space_storage_root_test")
        .join(format!("{name}_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i % 251) as u8 ^ seed ^ (i / 3001) as u8)
        .collect()
}

async fn open(db: &Path, root: &Path) -> anyhow::Result<(CapsuleRegistry, RegistryPipelineHandle)> {
    let registry = CapsuleRegistry::open(db)?;
    let storage = AutoFsBackend::open(root).await?;
    let handle = registry_pipeline_from_fs_backend(storage, registry.clone())?;
    Ok((registry, handle))
}

fn segment_file(root: &Path, segment: SegmentId) -> PathBuf {
    root.join("segments").join(format!("{}.seg", segment.0))
}

fn flip(path: &Path, at: usize) {
    let mut bytes = fs::read(path).unwrap();
    bytes[at] ^= 0x10;
    fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn light_scrub_finds_damaged_segment_files() {
    let dir = scratch("scrub");
    let root = dir.join("storage");
    let (registry, mut handle) = open(&dir.join("space.db"), &root).await.unwrap();
    let mut segments = Vec::new();
    for seed in 1..=3 {
        let id = handle
            .write_capsule(&patterned(100_000, seed), &Policy::default())
            .await
            .unwrap();
        segments.push(registry.get_capsule(id).unwrap().unwrap().segments[0]);
    }
    assert!(handle.scrub_all(ScrubKind::Light).await.errors.is_empty());

    // A stored byte in one segment, a descriptor byte in another.
    let body = segment_file(&root, segments[0]);
    flip(&body, fs::metadata(&body).unwrap().len() as usize / 2);
    flip(&segment_file(&root, segments[1]), 60);

    let report = handle.scrub_all(ScrubKind::Light).await;
    let mut damaged: Vec<SegmentId> = report
        .errors
        .iter()
        .map(|result| match result {
            ScrubResult::ContentCorrupted { segment } => *segment,
            other => panic!("expected corruption, got {other:?}"),
        })
        .collect();
    damaged.sort_by_key(|s| s.0);
    assert_eq!(damaged, segments[..2].to_vec());
    assert_eq!(report.segments_checked, 3);

    drop((registry, handle));
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_journal_refuses_another_stores_root() {
    let dir = scratch("pairing");
    {
        let (_registry, mut handle) = open(&dir.join("space.db"), &dir.join("storage"))
            .await
            .unwrap();
        handle
            .write_capsule(&patterned(10_000, 4), &Policy::default())
            .await
            .unwrap();
    }
    let err = open(&dir.join("space.db"), &dir.join("other"))
        .await
        .err()
        .expect("a journal with capsules opened with an empty root");
    assert!(err.to_string().contains("different stores"), "{err:#}");

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_lost_index_never_reuses_a_segment_id() {
    let dir = scratch("floor");
    let root = dir.join("storage");
    let first = {
        let (registry, mut handle) = open(&dir.join("space.db"), &root).await.unwrap();
        let id = handle
            .write_capsule(&patterned(10_000, 5), &Policy::default())
            .await
            .unwrap();
        registry.get_capsule(id).unwrap().unwrap().segments[0]
    };
    let before = fs::read(segment_file(&root, first)).unwrap();

    // Lose space.db and the journal: a new store is started over the root.
    fs::remove_dir_all(dir.join("space.db")).unwrap();
    let _ = fs::remove_file(dir.join("space.journal"));
    let (registry, mut handle) = open(&dir.join("space.db"), &root).await.unwrap();
    let id = handle
        .write_capsule(&patterned(10_000, 6), &Policy::default())
        .await
        .unwrap();
    let second = registry.get_capsule(id).unwrap().unwrap().segments[0];

    assert!(second.0 > first.0, "segment {} reused", second.0);
    assert_eq!(fs::read(segment_file(&root, first)).unwrap(), before);

    drop((registry, handle));
    let _ = fs::remove_dir_all(&dir);
}

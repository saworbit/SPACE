//! Scrubbing a real store (#97): light scrub checks the data log's record
//! checksums without a key, deep scrub checks MACs and content hashes, and a
//! scheduled scrub reports what it found. The schedule is kept in space.db,
//! so a restart only scrubs segments that are due (#134).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use capsule_registry::modular_pipeline::{
    registry_pipeline_from_log, registry_pipeline_from_log_with_key, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::scrub::{ScrubConfig, ScrubKind, ScrubResult, ScrubState, ScrubTimes};
use common::{CapsuleId, Policy, SegmentId};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use uuid::Uuid;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("space_scrub_test")
        .join(format!("{name}_{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn key() -> Arc<Mutex<KeyManager>> {
    Arc::new(Mutex::new(KeyManager::new([0x5Au8; MASTER_KEY_SIZE])))
}

fn open(dir: &Path, with_key: bool) -> (CapsuleRegistry, NvramLog, RegistryPipelineHandle) {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let handle = if with_key {
        registry_pipeline_from_log_with_key(log.clone(), registry.clone(), key()).unwrap()
    } else {
        registry_pipeline_from_log(log.clone(), registry.clone()).unwrap()
    };
    (registry, log, handle)
}

fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i % 251) as u8 ^ seed ^ (i / 3001) as u8)
        .collect()
}

/// Flip one byte in the middle of a segment's stored bytes.
fn rot(dir: &Path, log: &NvramLog, segment: SegmentId) {
    let meta = log.get_segment_metadata(segment).unwrap();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("space.nvram"))
        .unwrap();
    let at = meta.offset + u64::from(meta.len) / 2;
    let mut byte = [0u8];
    file.seek(SeekFrom::Start(at)).unwrap();
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(&[byte[0] ^ 0x40]).unwrap();
    file.sync_all().unwrap();
}

/// A plain capsule and an encrypted one, each with one damaged segment, and
/// a healthy capsule written after them.
async fn damaged_store(dir: &Path) -> (CapsuleId, CapsuleId) {
    let (registry, log, mut handle) = open(dir, true);
    let plain = handle
        .write_capsule(&patterned(100_000, 1), &Policy::default())
        .await
        .unwrap();
    let encrypted_policy = handle.default_policy();
    let encrypted = handle
        .write_capsule(&patterned(100_000, 2), &encrypted_policy)
        .await
        .unwrap();
    handle
        .write_capsule(&patterned(100_000, 3), &Policy::default())
        .await
        .unwrap();
    for id in [plain, encrypted] {
        rot(dir, &log, registry.lookup(id).unwrap().segments[0]);
    }
    (plain, encrypted)
}

fn segments_of(registry: &CapsuleRegistry, id: CapsuleId) -> Vec<SegmentId> {
    registry.lookup(id).unwrap().segments
}

#[tokio::test]
async fn light_scrub_finds_damage_without_the_key_and_deep_scrub_says_what() {
    let dir = scratch("damage");
    let (plain, encrypted) = damaged_store(&dir).await;

    // Without the key: record checksums still catch both.
    let (registry, _log, handle) = open(&dir, false);
    let light = handle.scrub_all(ScrubKind::Light).await;
    assert_eq!(light.segments_checked, 3);
    assert_eq!(light.content_failures, 2, "{:?}", light.errors);
    let mut found: Vec<SegmentId> = light
        .errors
        .iter()
        .filter_map(ScrubResult::segment)
        .collect();
    found.sort();
    let mut damaged = [
        segments_of(&registry, plain),
        segments_of(&registry, encrypted),
    ]
    .concat();
    damaged.sort();
    assert_eq!(found, damaged);

    // Deep scrub without the key can't check the encrypted segment's MAC.
    let deep = handle.scrub_all(ScrubKind::Deep).await;
    assert_eq!(deep.content_failures, 1);
    assert_eq!(deep.segments_skipped, 1);
    drop((registry, handle));

    // With it, the MAC check fails too.
    let (registry, _log, handle) = open(&dir, true);
    let deep = handle.scrub_all(ScrubKind::Deep).await;
    assert_eq!(deep.segments_skipped, 0);
    assert_eq!(deep.content_failures, 1);
    assert_eq!(deep.mac_failures, 1);
    let encrypted_segment = segments_of(&registry, encrypted)[0];
    assert!(deep.errors.iter().any(
        |e| matches!(e, ScrubResult::MacMismatch { segment } if *segment == encrypted_segment)
    ));

    drop((registry, handle));
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_scheduled_scrub_reports_damage_on_a_live_store() {
    let dir = scratch("scheduled");
    damaged_store(&dir).await;

    let (_registry, _log, handle) = open(&dir, true);
    // Long intervals: one deep cycle at startup, then nothing for an hour.
    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(3600),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let (task, mut state) = handle.spawn_scrub(config);
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            state.changed().await.unwrap();
            if let ScrubState::Completed { kind, errors } = *state.borrow() {
                return (kind, errors);
            }
        }
    })
    .await
    .expect("no scrub cycle completed");
    assert_eq!(outcome, (ScrubKind::Deep, 2));
    task.abort();

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_healthy_store_scrubs_clean() {
    let dir = scratch("healthy");
    let (_registry, _log, mut handle) = open(&dir, true);
    let policy = handle.default_policy();
    handle
        .write_capsule(&patterned(300_000, 4), &policy)
        .await
        .unwrap();
    handle
        .write_capsule(&patterned(300_000, 5), &Policy::default())
        .await
        .unwrap();
    for kind in [ScrubKind::Light, ScrubKind::Deep] {
        let report = handle.scrub_all(kind).await;
        assert_eq!(report.segments_checked, 2, "{kind:?}");
        assert!(report.errors.is_empty(), "{kind:?}: {:?}", report.errors);
        assert!(report.bytes_checked > 0);
    }
    drop(handle);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn after_a_restart_only_due_segments_are_scrubbed() {
    let dir = scratch("restart");
    let scrubbed_before = {
        let (registry, log, mut handle) = open(&dir, false);
        let id = handle
            .write_capsule(&patterned(100_000, 3), &Policy::default())
            .await
            .unwrap();
        assert!(handle.scrub_all(ScrubKind::Deep).await.errors.is_empty());
        let segments = registry.get_capsule(id).unwrap().unwrap().segments;
        drop((registry, log, handle));
        segments
    };

    let (registry, _log, mut handle) = open(&dir, false);
    let before: HashMap<SegmentId, ScrubTimes> = registry
        .scrub_schedule()
        .load()
        .unwrap()
        .into_iter()
        .collect();
    for segment in &scrubbed_before {
        assert!(
            before[segment].deep.is_some(),
            "spacectl scrub's results weren't kept"
        );
    }
    let id = handle
        .write_capsule(&patterned(100_000, 4), &Policy::default())
        .await
        .unwrap();
    let written_after = registry.get_capsule(id).unwrap().unwrap().segments;

    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(7200),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let (task, mut state) = handle.spawn_scrub(config);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(*state.borrow_and_update(), ScrubState::Completed { .. }) {
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("the first scheduled cycle didn't finish");
    task.abort();

    let after: HashMap<SegmentId, ScrubTimes> = registry
        .scrub_schedule()
        .load()
        .unwrap()
        .into_iter()
        .collect();
    for segment in &scrubbed_before {
        assert_eq!(
            after[segment], before[segment],
            "a segment scrubbed before the restart was scrubbed again"
        );
    }
    for segment in &written_after {
        assert!(
            after.get(segment).and_then(|t| t.deep).is_some(),
            "a segment written after the restart wasn't scrubbed"
        );
    }

    drop((registry, handle));
    let _ = fs::remove_dir_all(&dir);
}

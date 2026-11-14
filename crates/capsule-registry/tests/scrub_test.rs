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

/// Damage a scheduled scrub found is reported again after a restart, though
/// the damaged segments aren't due to be checked again yet (#195).
#[tokio::test]
async fn damage_found_before_a_restart_is_reported_after_it() {
    let dir = scratch("restart_damage");
    damaged_store(&dir).await;
    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(3600),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let mut outcomes = Vec::new();
    for _run in 0..2 {
        let (registry, log, handle) = open(&dir, true);
        let (task, mut state) = handle.spawn_scrub(config.clone());
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                state.changed().await.unwrap();
                if let ScrubState::Completed { kind, errors } = *state.borrow() {
                    return (kind, errors);
                }
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        outcomes.push(outcome);
        drop((registry, log, handle));
    }
    assert_eq!(outcomes[0], (ScrubKind::Deep, 2));
    assert_eq!(
        outcomes[1],
        (ScrubKind::Deep, 2),
        "damage found before the restart is reported after it"
    );

    // The damage is kept with the schedule in space.db.
    let (registry, _log, _handle) = open(&dir, true);
    let damaged = registry
        .scrub_schedule()
        .load()
        .unwrap()
        .into_iter()
        .filter(|(_, times)| times.damage.is_some())
        .count();
    assert_eq!(damaged, 2);
}

/// A second capsule over `of`'s segments, listed in reverse, with `of`'s
/// digest: each segment is sound, but they don't make up its bytes.
fn reversed(registry: &CapsuleRegistry, of: CapsuleId) -> CapsuleId {
    use common::format::{CapsuleManifest, JournalRecord, ManifestEntry};
    const SEGMENT: u64 = 4 << 20;
    let capsule = registry.lookup(of).unwrap();
    assert!(capsule.segments.len() > 1);
    let mut left = capsule.size;
    let mut entries: Vec<ManifestEntry> = capsule
        .segments
        .iter()
        .map(|segment| {
            let len = left.min(SEGMENT);
            left -= len;
            ManifestEntry {
                segment_id: *segment,
                plain_len: len as u32,
                content_hash: [0; 32],
            }
        })
        .collect();
    entries.reverse();
    let id = CapsuleId::new();
    let manifest = CapsuleManifest {
        capsule_id: id,
        created_at: 0,
        size: capsule.size,
        digest: capsule.digest.unwrap(),
        dedupe: false,
        requires_encryption: capsule.policy.encryption.is_enabled(),
        deduped_bytes: 0,
        policy: capsule.policy.clone(),
        entries,
    };
    registry
        .commit(vec![JournalRecord::Capsule(Box::new(manifest))])
        .unwrap();
    id
}

/// A deep scrub reads each capsule whole and checks it against its digest,
/// so it finds a capsule whose segments are each sound but listed in the
/// wrong order. A light scrub doesn't read capsules (#140).
#[tokio::test]
async fn a_deep_scrub_finds_a_capsule_whose_segments_are_out_of_order() {
    use capsule_registry::modular_pipeline::{scrub_all, spawn_scrub};
    let dir = scratch("capsule_order");
    let (registry, _log, mut handle) = open(&dir, true);
    let data = patterned((9 << 20) + 7, 4);
    let good = handle
        .write_capsule(&data, &handle.default_policy())
        .await
        .unwrap();
    let bad = reversed(&registry, good);
    let pipeline = Arc::new(tokio::sync::Mutex::new(handle));

    // In the background, after the first deep cycle, with the capsules
    // holding the segments it scrubbed: on a new schedule, all of them.
    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(3600),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let (task, mut state) = spawn_scrub(&pipeline, config).await;
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            state.changed().await.unwrap();
            if let ScrubState::Completed { kind, errors } = *state.borrow() {
                return (kind, errors);
            }
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(outcome, (ScrubKind::Deep, 1));

    let report = scrub_all(&pipeline, ScrubKind::Deep).await;
    assert_eq!(report.mac_failures + report.content_failures, 0);
    assert_eq!(report.capsules_checked, 2);
    assert_eq!(report.capsule_failures, 1);
    assert!(
        matches!(&report.errors[..], [ScrubResult::CapsuleCorrupted { capsule, reason }]
            if *capsule == bad && reason.contains("digest")),
        "{:?}",
        report.errors
    );
    let light = scrub_all(&pipeline, ScrubKind::Light).await;
    assert_eq!((light.capsules_checked, light.capsule_failures), (0, 0));

    // Every segment is now recorded, so a restarted background scrub's
    // first deep cycle has none due, and reads no capsule again.
    let config = ScrubConfig {
        light_interval: Duration::from_secs(3600),
        deep_interval: Duration::from_secs(3600),
        inter_segment_delay: Duration::ZERO,
        ..ScrubConfig::default()
    };
    let (task, mut state) = spawn_scrub(&pipeline, config).await;
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            state.changed().await.unwrap();
            if let ScrubState::Completed { kind, errors } = *state.borrow() {
                return (kind, errors);
            }
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(outcome, (ScrubKind::Deep, 0));
    drop((registry, pipeline));

    // Without the key, the encrypted capsules are skipped and counted.
    let (_registry, _log, handle) = open(&dir, false);
    let pipeline = Arc::new(tokio::sync::Mutex::new(handle));
    let report = scrub_all(&pipeline, ScrubKind::Deep).await;
    assert_eq!(
        (report.capsules_checked, report.capsules_without_key),
        (0, 2)
    );
}

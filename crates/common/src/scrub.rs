//! Background scrub scheduler for stored segment integrity verification.
//!
//! Implements a two-level scrubbing model:
//! - **Light scrub**: Verify segment metadata (size, existence) — fast.
//! - **Deep scrub**: Re-read data and verify content hash / MAC — catches bit-rot.
//!
//! The scrub task runs as a background `tokio::spawn` loop, processing a
//! bounded number of segments per cycle to avoid starving foreground IO.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::SegmentId;

/// Configuration for the background scrub scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrubConfig {
    /// Interval between light scrub cycles.
    pub light_interval: Duration,
    /// Interval between deep scrub cycles.
    pub deep_interval: Duration,
    /// Maximum segments to scrub per cycle (prevents IO starvation).
    pub max_segments_per_cycle: usize,
    /// Pause between individual segment checks to yield back to foreground IO.
    pub inter_segment_delay: Duration,
    /// Maximum I/O bandwidth consumed by scrubbing, in bytes per second.
    ///
    /// The executor paces reads so the cumulative byte rate stays at or below
    /// this limit, sleeping only as long as needed to honour it.  This is
    /// segment-size-aware: large segments are automatically throttled more
    /// than small ones, unlike a fixed `inter_segment_delay`.
    ///
    /// `None` means unlimited (scrub as fast as the backend allows).
    #[serde(default)]
    pub max_bytes_per_sec: Option<u64>,
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            light_interval: Duration::from_secs(24 * 3600), // 1 day
            deep_interval: Duration::from_secs(7 * 24 * 3600), // 1 week
            max_segments_per_cycle: 1024,
            inter_segment_delay: Duration::from_millis(5),
            max_bytes_per_sec: None,
        }
    }
}

/// Outcome of scrubbing a single segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScrubResult {
    /// Segment is healthy — integrity was positively verified.
    Ok,
    /// Segment metadata mismatch (size, missing file).
    MetadataMismatch {
        segment: SegmentId,
        expected_len: u32,
        actual_len: u32,
    },
    /// Content hash does not match stored hash (bit-rot detected).
    ContentCorrupted { segment: SegmentId },
    /// MAC verification failed.
    MacMismatch { segment: SegmentId },
    /// Segment data could not be read.
    ReadError { segment: SegmentId, error: String },
    /// Integrity could not be verified right now due to a transient condition
    /// (e.g. no key manager available for an encrypted segment).
    ///
    /// Unlike `Ok`, a `Skipped` result does **not** advance the segment's
    /// scrub timestamp, so it will be retried at the next cycle.
    Skipped { reason: String },
}

impl ScrubResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, ScrubResult::Ok)
    }

    pub fn is_skipped(&self) -> bool {
        matches!(self, ScrubResult::Skipped { .. })
    }

    /// The segment this result is about, if it names one.
    pub fn segment(&self) -> Option<SegmentId> {
        match self {
            ScrubResult::MetadataMismatch { segment, .. }
            | ScrubResult::ContentCorrupted { segment }
            | ScrubResult::MacMismatch { segment }
            | ScrubResult::ReadError { segment, .. } => Some(*segment),
            ScrubResult::Ok | ScrubResult::Skipped { .. } => None,
        }
    }

    /// Whether this result should advance the segment's scrub schedule
    /// timestamp. A failure does, so a damaged segment isn't checked again
    /// every cycle, but the damage is kept with the schedule
    /// ([`ScrubSchedule::note`]) and reported until a scrub passes the
    /// segment (#195).
    ///
    /// Returns `false` for transient outcomes — the segment remains due so
    /// it will be retried at the next cycle:
    /// - `Skipped`: verification blocked by a temporary condition (e.g. no
    ///   key manager).
    /// - `ReadError`: I/O may be transient; keep retrying until the failure
    ///   either clears or escalates to a definitive integrity finding.
    ///
    /// Returns `true` for definitive outcomes (pass or fail) whose result
    /// won't change by re-reading the same bytes immediately:
    /// - `Ok`, `MetadataMismatch`, `ContentCorrupted`, `MacMismatch`.
    pub fn should_record_schedule(&self) -> bool {
        !matches!(
            self,
            ScrubResult::Skipped { .. } | ScrubResult::ReadError { .. }
        )
    }
}

/// Observable state of the background scrub task.
///
/// Published via a `tokio::sync::watch` channel by the scrub executor's
/// `spawn_background` task so monitoring consumers can track state changes
/// without polling — analogous to TrueNAS's scrub state machine
/// (WAITING → SCANNING → FINISHED/CANCELED).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrubState {
    /// No scrub cycle is currently running.
    Idle,
    /// A scrub cycle is actively running.
    Running(ScrubKind),
    /// The most recent cycle completed.  `errors` counts **definitive**
    /// integrity failures only (`MetadataMismatch`, `MacMismatch`,
    /// `ContentCorrupted`); transient `ReadError`s are excluded so this
    /// field is suitable for alerting thresholds.  Persists until the next
    /// cycle's `Running` replaces it (never immediately overwritten by
    /// `Idle`).
    Completed { kind: ScrubKind, errors: usize },
}

/// Aggregate report from a scrub cycle.
///
/// Marked `#[non_exhaustive]` so that new diagnostic counters can be added
/// without breaking downstream code that constructs this struct directly.
/// Callers that only read fields are unaffected.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScrubReport {
    /// Segments that completed a check attempt this cycle: verified clean,
    /// found corrupt, or failed to read (`ReadError`). Does **not** include
    /// segments that were `Skipped` due to a transient condition.
    pub segments_checked: usize,
    /// Segments that could not be verified due to a transient condition
    /// (`Skipped`) and were not recorded in the scrub schedule - they remain
    /// due and will be retried next cycle.
    #[serde(default)]
    pub segments_skipped: usize,
    /// Segments for which a schedule timestamp was actually advanced this
    /// cycle (i.e. `should_record_schedule()` returned `true`). Used by the
    /// background loop to detect all-transient cycles and avoid spinning.
    #[serde(default)]
    pub segments_recorded: usize,
    pub errors: Vec<ScrubResult>,
    /// Every finding not yet cleared at the end of the cycle, this cycle's
    /// and earlier ones', by segment (#195).
    #[serde(default)]
    pub known_damage: Vec<ScrubResult>,
    pub duration: Duration,
    pub kind: ScrubKind,
    /// MAC verification failures (encrypted segments whose integrity tag did
    /// not match). Subset of `errors`.
    #[serde(default)]
    pub mac_failures: usize,
    /// Content hash mismatches (unencrypted segments with detected bit-rot).
    /// Subset of `errors`.
    #[serde(default)]
    pub content_failures: usize,
    /// Read I/O errors — transient failures that prevent verification.
    /// Subset of `errors`.
    #[serde(default)]
    pub read_errors: usize,
    /// Byte-length mismatches between stored data and the `len` field in
    /// segment metadata (truncation, missing data). Subset of `errors`.
    #[serde(default)]
    pub metadata_failures: usize,
    /// Total bytes read from the backend during this cycle.
    ///
    /// Useful for measuring scrub throughput and validating that
    /// `max_bytes_per_sec` throttling is working as expected.
    #[serde(default)]
    pub bytes_checked: u64,
    /// Segments that verified clean only via the pre-fix bare-BLAKE3 hash
    /// (the compatibility-window fallback in `dedup::verify_content_hash`).
    ///
    /// Operators can watch this counter trend to zero before retiring the
    /// fallback. Not an error — these segments are integrity-clean — but
    /// they should be rewritten under the algo-domain-separated scheme.
    #[serde(default)]
    pub legacy_hash_hits: u64,
    /// Segments that were due but left for a later cycle because this one
    /// reached `max_segments_per_cycle`.
    #[serde(default)]
    pub segments_deferred: usize,
}

/// Whether a scrub cycle is light (length-only) or deep (content verification).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScrubKind {
    #[default]
    Light,
    Deep,
}

/// When a segment was last scrubbed, and any damage a scrub found in it,
/// as kept between runs by a [`ScheduleStore`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrubTimes {
    pub light: Option<SystemTime>,
    pub deep: Option<SystemTime>,
    pub damage: Option<Damage>,
}

/// Damage a scrub found in a segment (#195). It's kept, and reported every
/// cycle and after restarts, until a scrub at least as thorough as the one
/// that found it passes the segment, or the segment is gone. A light scrub
/// can't see a bad MAC, so its pass doesn't clear what a deep one found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub found_by: ScrubKind,
    pub what: DamageKind,
}

/// What was wrong with a damaged segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageKind {
    MetadataMismatch { expected_len: u32, actual_len: u32 },
    ContentCorrupted,
    MacMismatch,
}

impl Damage {
    /// The damage a failed check found, or `None` for a result that isn't
    /// a definitive failure.
    pub fn of(found_by: ScrubKind, result: &ScrubResult) -> Option<Self> {
        let what = match *result {
            ScrubResult::MetadataMismatch {
                expected_len,
                actual_len,
                ..
            } => DamageKind::MetadataMismatch {
                expected_len,
                actual_len,
            },
            ScrubResult::ContentCorrupted { .. } => DamageKind::ContentCorrupted,
            ScrubResult::MacMismatch { .. } => DamageKind::MacMismatch,
            _ => return None,
        };
        Some(Self { found_by, what })
    }

    /// The finding as a scrub result for `segment`.
    pub fn result(&self, segment: SegmentId) -> ScrubResult {
        match self.what {
            DamageKind::MetadataMismatch {
                expected_len,
                actual_len,
            } => ScrubResult::MetadataMismatch {
                segment,
                expected_len,
                actual_len,
            },
            DamageKind::ContentCorrupted => ScrubResult::ContentCorrupted { segment },
            DamageKind::MacMismatch => ScrubResult::MacMismatch { segment },
        }
    }
}

/// Somewhere to keep the scrub schedule between runs, so a restart doesn't
/// scrub the whole store again (#134). Losing it only costs one full scrub.
pub trait ScheduleStore: Send + Sync {
    /// Every segment's recorded times.
    fn load(&self) -> anyhow::Result<Vec<(SegmentId, ScrubTimes)>>;
    /// Apply changes: `Some` sets a segment's times, `None` forgets it.
    fn save(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> anyhow::Result<()>;
}

/// Tracks per-segment scrub history for scheduling decisions.
///
/// Times are wall-clock, so they can be kept across restarts. A time in the
/// future, from a clock set back, counts as due.
#[derive(Debug, Clone, Default)]
pub struct ScrubSchedule {
    /// Last light scrub time per segment.
    pub last_light: BTreeMap<SegmentId, SystemTime>,
    /// Last deep scrub time per segment.
    pub last_deep: BTreeMap<SegmentId, SystemTime>,
    /// Damage found in segments and not yet cleared (#195).
    pub damage: BTreeMap<SegmentId, Damage>,
    /// Segments whose times changed since [`ScrubSchedule::take_changes`].
    changed: BTreeSet<SegmentId>,
}

fn is_due(last: Option<&SystemTime>, interval: Duration, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(t) => now.duration_since(*t).map_or(true, |age| age >= interval),
    }
}

impl ScrubSchedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// A schedule holding times loaded from a [`ScheduleStore`], with no
    /// pending changes.
    pub fn from_times(times: impl IntoIterator<Item = (SegmentId, ScrubTimes)>) -> Self {
        let mut schedule = Self::new();
        for (segment, t) in times {
            if let Some(light) = t.light {
                schedule.last_light.insert(segment, light);
            }
            if let Some(deep) = t.deep {
                schedule.last_deep.insert(segment, deep);
            }
            if let Some(damage) = t.damage {
                schedule.damage.insert(segment, damage);
            }
        }
        schedule
    }

    /// Keep or clear damage after a definitive check of `segment` by a
    /// `kind` scrub: a failure is kept, and a pass clears damage a scrub no
    /// more thorough than `kind` found.
    pub fn note(&mut self, segment: SegmentId, kind: ScrubKind, result: &ScrubResult) {
        if let Some(damage) = Damage::of(kind, result) {
            if self.damage.insert(segment, damage) != Some(damage) {
                self.changed.insert(segment);
            }
        } else if result.is_ok() {
            let clears =
                |found_by: ScrubKind| kind == ScrubKind::Deep || found_by == ScrubKind::Light;
            if self
                .damage
                .get(&segment)
                .is_some_and(|d| clears(d.found_by))
            {
                self.damage.remove(&segment);
                self.changed.insert(segment);
            }
        }
    }

    /// Every finding not yet cleared, by segment.
    pub fn damage_results(&self) -> Vec<ScrubResult> {
        self.damage
            .iter()
            .map(|(segment, damage)| damage.result(*segment))
            .collect()
    }

    /// Return segment IDs that are due for a light scrub.
    pub fn due_for_light(&self, all: &[SegmentId], config: &ScrubConfig) -> Vec<SegmentId> {
        let now = SystemTime::now();
        all.iter()
            .filter(|id| is_due(self.last_light.get(id), config.light_interval, now))
            .copied()
            .collect()
    }

    /// Return segment IDs that are due for a deep scrub.
    pub fn due_for_deep(&self, all: &[SegmentId], config: &ScrubConfig) -> Vec<SegmentId> {
        let now = SystemTime::now();
        all.iter()
            .filter(|id| is_due(self.last_deep.get(id), config.deep_interval, now))
            .copied()
            .collect()
    }

    /// The earliest time after `now` at which a recorded segment falls due
    /// for `kind`, if any does. Segments already due aren't counted: the
    /// cycle that just ran either checked them or couldn't.
    pub fn next_due(
        &self,
        kind: ScrubKind,
        config: &ScrubConfig,
        now: SystemTime,
    ) -> Option<SystemTime> {
        let (times, interval) = match kind {
            ScrubKind::Light => (&self.last_light, config.light_interval),
            ScrubKind::Deep => (&self.last_deep, config.deep_interval),
        };
        times
            .values()
            .filter_map(|t| t.checked_add(interval))
            .filter(|due| *due > now)
            .min()
    }

    /// Forget segments not in `present`, such as ones deleted since they were
    /// scrubbed, so a long-running schedule doesn't grow with every segment
    /// the store has ever held.
    pub fn retain(&mut self, present: &[SegmentId]) {
        let present: std::collections::HashSet<&SegmentId> = present.iter().collect();
        let gone: Vec<SegmentId> = self
            .last_light
            .keys()
            .chain(self.last_deep.keys())
            .chain(self.damage.keys())
            .filter(|id| !present.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.last_light.remove(&id);
            self.last_deep.remove(&id);
            self.damage.remove(&id);
            self.changed.insert(id);
        }
    }

    /// Record that a segment was scrubbed.
    pub fn record(&mut self, segment: SegmentId, kind: ScrubKind) {
        let now = SystemTime::now();
        match kind {
            ScrubKind::Light => {
                self.last_light.insert(segment, now);
            }
            ScrubKind::Deep => {
                self.last_deep.insert(segment, now);
                // Deep implies light.
                self.last_light.insert(segment, now);
            }
        }
        self.changed.insert(segment);
    }

    /// The changes since the last call, for [`ScheduleStore::save`]: each
    /// changed segment's times, or `None` for one that was forgotten.
    pub fn take_changes(&mut self) -> Vec<(SegmentId, Option<ScrubTimes>)> {
        std::mem::take(&mut self.changed)
            .into_iter()
            .map(|id| {
                let times = ScrubTimes {
                    light: self.last_light.get(&id).copied(),
                    deep: self.last_deep.get(&id).copied(),
                    damage: self.damage.get(&id).copied(),
                };
                let kept = times.light.is_some() || times.deep.is_some() || times.damage.is_some();
                (id, kept.then_some(times))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_is_kept_until_a_scrub_as_thorough_passes_it() {
        let seg = SegmentId(7);
        let mac = ScrubResult::MacMismatch { segment: seg };
        let mut schedule = ScrubSchedule::new();
        schedule.note(seg, ScrubKind::Deep, &mac);
        assert_eq!(schedule.damage_results().len(), 1);
        // A light pass can't see a bad MAC.
        schedule.note(seg, ScrubKind::Light, &ScrubResult::Ok);
        assert!(matches!(
            schedule.damage_results()[..],
            [ScrubResult::MacMismatch { .. }]
        ));
        // A read error says nothing either way.
        let read = ScrubResult::ReadError {
            segment: seg,
            error: "busy".into(),
        };
        schedule.note(seg, ScrubKind::Deep, &read);
        assert_eq!(schedule.damage_results().len(), 1);

        // It survives a restart, and a deep pass clears it.
        let saved = schedule.take_changes();
        assert!(matches!(saved[..], [(s, Some(ScrubTimes { damage: Some(_), .. }))] if s == seg));
        let mut reloaded =
            ScrubSchedule::from_times(saved.into_iter().map(|(s, t)| (s, t.unwrap())));
        assert_eq!(reloaded.damage_results().len(), 1);
        reloaded.note(seg, ScrubKind::Deep, &ScrubResult::Ok);
        assert!(reloaded.damage_results().is_empty());
        assert_eq!(reloaded.take_changes(), [(seg, None)]);

        // What a light scrub found, a light pass clears; and a segment
        // that's gone takes its damage with it.
        let mut schedule = ScrubSchedule::new();
        let short = ScrubResult::MetadataMismatch {
            segment: seg,
            expected_len: 9,
            actual_len: 4,
        };
        schedule.note(seg, ScrubKind::Light, &short);
        schedule.note(
            SegmentId(8),
            ScrubKind::Light,
            &ScrubResult::ContentCorrupted {
                segment: SegmentId(8),
            },
        );
        schedule.note(seg, ScrubKind::Light, &ScrubResult::Ok);
        assert!(matches!(
            schedule.damage_results()[..],
            [ScrubResult::ContentCorrupted {
                segment: SegmentId(8)
            }]
        ));
        schedule.retain(&[seg]);
        assert!(schedule.damage_results().is_empty());
    }

    #[test]
    fn new_segments_are_always_due() {
        let schedule = ScrubSchedule::new();
        let config = ScrubConfig::default();
        let ids = vec![SegmentId(1), SegmentId(2), SegmentId(3)];

        let light = schedule.due_for_light(&ids, &config);
        let deep = schedule.due_for_deep(&ids, &config);

        assert_eq!(
            light.len(),
            3,
            "all new segments should be due for light scrub"
        );
        assert_eq!(
            deep.len(),
            3,
            "all new segments should be due for deep scrub"
        );
    }

    #[test]
    fn recently_scrubbed_segments_not_due() {
        let mut schedule = ScrubSchedule::new();
        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::from_secs(86400),
            ..Default::default()
        };
        let ids = vec![SegmentId(1), SegmentId(2)];

        schedule.record(SegmentId(1), ScrubKind::Deep);

        let light = schedule.due_for_light(&ids, &config);
        let deep = schedule.due_for_deep(&ids, &config);

        assert_eq!(light.len(), 1, "only un-scrubbed segment should be due");
        assert_eq!(light[0], SegmentId(2));
        assert_eq!(deep.len(), 1);
    }

    #[test]
    fn recording_and_forgetting_are_reported_as_changes() {
        let mut schedule = ScrubSchedule::new();
        schedule.record(SegmentId(1), ScrubKind::Light);
        schedule.record(SegmentId(2), ScrubKind::Deep);

        let changes = schedule.take_changes();
        assert_eq!(changes.len(), 2);
        let one = changes[0].1.unwrap();
        assert!(one.light.is_some() && one.deep.is_none());
        let two = changes[1].1.unwrap();
        assert_eq!(two.light, two.deep, "deep implies light");
        assert!(schedule.take_changes().is_empty(), "changes are taken once");

        schedule.retain(&[SegmentId(2)]);
        assert_eq!(schedule.take_changes(), vec![(SegmentId(1), None)]);
    }

    #[test]
    fn loaded_times_decide_what_is_due() {
        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::from_secs(86400),
            ..Default::default()
        };
        let now = SystemTime::now();
        let schedule = ScrubSchedule::from_times([
            (
                SegmentId(1),
                ScrubTimes {
                    light: Some(now - Duration::from_secs(60)),
                    deep: Some(now - Duration::from_secs(90_000)),
                    damage: None,
                },
            ),
            (
                SegmentId(2),
                ScrubTimes {
                    light: Some(now - Duration::from_secs(7200)),
                    deep: None,
                    damage: None,
                },
            ),
        ]);
        let mut loaded = schedule.clone();
        assert!(loaded.take_changes().is_empty());

        let ids = [SegmentId(1), SegmentId(2), SegmentId(3)];
        assert_eq!(
            schedule.due_for_light(&ids, &config),
            vec![SegmentId(2), SegmentId(3)]
        );
        assert_eq!(schedule.due_for_deep(&ids, &config), ids.to_vec());
    }

    #[test]
    fn a_time_in_the_future_counts_as_due() {
        let schedule = ScrubSchedule::from_times([(
            SegmentId(1),
            ScrubTimes {
                light: Some(SystemTime::now() + Duration::from_secs(3600)),
                deep: None,
                damage: None,
            },
        )]);
        let due = schedule.due_for_light(&[SegmentId(1)], &ScrubConfig::default());
        assert_eq!(due, vec![SegmentId(1)]);
    }

    #[test]
    fn next_due_is_the_earliest_time_still_ahead() {
        let config = ScrubConfig {
            light_interval: Duration::from_secs(100),
            ..Default::default()
        };
        let now = SystemTime::now();
        let schedule = ScrubSchedule::from_times([
            (
                SegmentId(1),
                ScrubTimes {
                    light: Some(now - Duration::from_secs(70)),
                    deep: None,
                    damage: None,
                },
            ),
            (
                SegmentId(2),
                ScrubTimes {
                    light: Some(now - Duration::from_secs(20)),
                    deep: None,
                    damage: None,
                },
            ),
            // Already overdue: not counted.
            (
                SegmentId(3),
                ScrubTimes {
                    light: Some(now - Duration::from_secs(500)),
                    deep: None,
                    damage: None,
                },
            ),
        ]);
        assert_eq!(
            schedule.next_due(ScrubKind::Light, &config, now),
            Some(now + Duration::from_secs(30))
        );
        assert_eq!(schedule.next_due(ScrubKind::Deep, &config, now), None);
    }

    // ── ScrubConfig defaults ────────────────────────────────────────

    #[test]
    fn config_defaults_are_reasonable() {
        let config = ScrubConfig::default();
        assert_eq!(config.light_interval, Duration::from_secs(24 * 3600));
        assert_eq!(config.deep_interval, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(config.max_segments_per_cycle, 1024);
        assert_eq!(config.inter_segment_delay, Duration::from_millis(5));
        assert_eq!(config.max_bytes_per_sec, None, "unlimited by default");
    }

    // ── ScrubResult variants ────────────────────────────────────────

    #[test]
    fn scrub_result_ok_is_ok() {
        assert!(ScrubResult::Ok.is_ok());
    }

    #[test]
    fn scrub_result_errors_are_not_ok() {
        let cases = vec![
            ScrubResult::MetadataMismatch {
                segment: SegmentId(0),
                expected_len: 100,
                actual_len: 80,
            },
            ScrubResult::ContentCorrupted {
                segment: SegmentId(1),
            },
            ScrubResult::MacMismatch {
                segment: SegmentId(2),
            },
            ScrubResult::ReadError {
                segment: SegmentId(3),
                error: "disk failure".into(),
            },
            ScrubResult::Skipped {
                reason: "no key manager".into(),
            },
        ];
        for case in &cases {
            assert!(!case.is_ok(), "{case:?} should not be Ok");
        }
    }

    #[test]
    fn skipped_is_skipped_not_ok() {
        let r = ScrubResult::Skipped {
            reason: "test".into(),
        };
        assert!(r.is_skipped());
        assert!(!r.is_ok());
    }

    // -- should_record_schedule ----------------------------------------------

    #[test]
    fn definitive_results_record_schedule() {
        let definitive = vec![
            ScrubResult::Ok,
            ScrubResult::MetadataMismatch {
                segment: SegmentId(0),
                expected_len: 4,
                actual_len: 2,
            },
            ScrubResult::ContentCorrupted {
                segment: SegmentId(1),
            },
            ScrubResult::MacMismatch {
                segment: SegmentId(2),
            },
        ];
        for r in &definitive {
            assert!(
                r.should_record_schedule(),
                "{r:?} should advance the schedule"
            );
        }
    }

    #[test]
    fn transient_results_do_not_record_schedule() {
        let transient = vec![
            ScrubResult::ReadError {
                segment: SegmentId(1),
                error: "I/O error".into(),
            },
            ScrubResult::Skipped {
                reason: "no key manager".into(),
            },
        ];
        for r in &transient {
            assert!(
                !r.should_record_schedule(),
                "{r:?} should NOT advance the schedule"
            );
        }
    }

    // -- ScrubReport ---------------------------------------------------------

    #[test]
    fn scrub_report_default_is_empty() {
        let report = ScrubReport::default();
        assert_eq!(report.segments_checked, 0);
        assert_eq!(report.segments_skipped, 0);
        assert_eq!(report.segments_recorded, 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.duration, Duration::ZERO);
        assert_eq!(report.kind, ScrubKind::Light);
        assert_eq!(report.mac_failures, 0);
        assert_eq!(report.content_failures, 0);
        assert_eq!(report.read_errors, 0);
        assert_eq!(report.metadata_failures, 0);
        assert_eq!(report.bytes_checked, 0);
    }

    #[test]
    fn scrub_state_transitions() {
        assert_eq!(ScrubState::Idle, ScrubState::Idle);
        assert_ne!(ScrubState::Idle, ScrubState::Running(ScrubKind::Light));

        let completed = ScrubState::Completed {
            kind: ScrubKind::Deep,
            errors: 2,
        };
        assert_ne!(completed, ScrubState::Idle);
        if let ScrubState::Completed { kind, errors } = completed {
            assert_eq!(kind, ScrubKind::Deep);
            assert_eq!(errors, 2);
        } else {
            panic!("expected Completed");
        }
    }

    #[test]
    fn scrub_report_accumulates_errors() {
        let mut report = ScrubReport {
            segments_checked: 5,
            kind: ScrubKind::Deep,
            ..Default::default()
        };
        report.errors.push(ScrubResult::ContentCorrupted {
            segment: SegmentId(10),
        });
        report.errors.push(ScrubResult::MacMismatch {
            segment: SegmentId(11),
        });

        assert_eq!(report.errors.len(), 2);
        assert_eq!(report.kind, ScrubKind::Deep);
    }

    // ── ScrubKind ───────────────────────────────────────────────────

    #[test]
    fn scrub_kind_default_is_light() {
        assert_eq!(ScrubKind::default(), ScrubKind::Light);
    }

    #[test]
    fn scrub_kind_equality() {
        assert_eq!(ScrubKind::Light, ScrubKind::Light);
        assert_eq!(ScrubKind::Deep, ScrubKind::Deep);
        assert_ne!(ScrubKind::Light, ScrubKind::Deep);
    }

    // ── Schedule: deep implies light ────────────────────────────────

    #[test]
    fn deep_scrub_records_light_timestamp_too() {
        let mut schedule = ScrubSchedule::new();
        schedule.record(SegmentId(1), ScrubKind::Deep);

        assert!(
            schedule.last_light.contains_key(&SegmentId(1)),
            "deep scrub should also record a light-scrub timestamp"
        );
        assert!(schedule.last_deep.contains_key(&SegmentId(1)));
    }

    #[test]
    fn light_scrub_does_not_record_deep() {
        let mut schedule = ScrubSchedule::new();
        schedule.record(SegmentId(1), ScrubKind::Light);

        assert!(schedule.last_light.contains_key(&SegmentId(1)));
        assert!(
            !schedule.last_deep.contains_key(&SegmentId(1)),
            "light scrub should NOT record a deep-scrub timestamp"
        );
    }

    // ── Schedule: empty segment list ────────────────────────────────

    #[test]
    fn no_segments_means_nothing_due() {
        let schedule = ScrubSchedule::new();
        let config = ScrubConfig::default();
        let ids: Vec<SegmentId> = vec![];

        assert!(schedule.due_for_light(&ids, &config).is_empty());
        assert!(schedule.due_for_deep(&ids, &config).is_empty());
    }

    // ── Schedule: many segments ─────────────────────────────────────

    #[test]
    fn due_for_scrub_scales_with_segment_count() {
        let schedule = ScrubSchedule::new();
        let config = ScrubConfig::default();
        let ids: Vec<SegmentId> = (0..100).map(SegmentId).collect();

        let light = schedule.due_for_light(&ids, &config);
        let deep = schedule.due_for_deep(&ids, &config);

        assert_eq!(light.len(), 100, "all unseen segments should be due");
        assert_eq!(deep.len(), 100);
    }

    // ── ScrubConfig serialization round-trip ────────────────────────

    #[test]
    fn scrub_config_serde_roundtrip() {
        let config = ScrubConfig {
            light_interval: Duration::from_secs(600),
            deep_interval: Duration::from_secs(3600),
            max_segments_per_cycle: 256,
            inter_segment_delay: Duration::from_millis(10),
            max_bytes_per_sec: Some(50 * 1024 * 1024), // 50 MB/s
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: ScrubConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.max_segments_per_cycle, 256);
        assert_eq!(restored.max_bytes_per_sec, Some(50 * 1024 * 1024));
    }
}

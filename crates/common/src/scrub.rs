//! Background scrub scheduler for stored segment integrity verification.
//!
//! Implements a two-level scrubbing model:
//! - **Light scrub**: Verify segment metadata (size, existence) — fast.
//! - **Deep scrub**: Re-read data and verify content hash / MAC — catches bit-rot.
//!
//! The scrub task runs as a background `tokio::spawn` loop, processing a
//! bounded number of segments per cycle to avoid starving foreground IO.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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
    MetadataMismatch { expected_len: u32, actual_len: u32 },
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

    /// Whether this result should advance the segment's scrub schedule
    /// timestamp.
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
}

/// Whether a scrub cycle is light (length-only) or deep (content verification).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScrubKind {
    #[default]
    Light,
    Deep,
}

/// Tracks per-segment scrub history for scheduling decisions.
#[derive(Debug, Clone, Default)]
pub struct ScrubSchedule {
    /// Last light scrub time per segment.
    pub last_light: BTreeMap<SegmentId, Instant>,
    /// Last deep scrub time per segment.
    pub last_deep: BTreeMap<SegmentId, Instant>,
}

impl ScrubSchedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return segment IDs that are due for a light scrub.
    pub fn due_for_light(&self, all: &[SegmentId], config: &ScrubConfig) -> Vec<SegmentId> {
        let now = Instant::now();
        all.iter()
            .filter(|id| {
                self.last_light
                    .get(id)
                    .is_none_or(|t| now.duration_since(*t) >= config.light_interval)
            })
            .copied()
            .collect()
    }

    /// Return segment IDs that are due for a deep scrub.
    pub fn due_for_deep(&self, all: &[SegmentId], config: &ScrubConfig) -> Vec<SegmentId> {
        let now = Instant::now();
        all.iter()
            .filter(|id| {
                self.last_deep
                    .get(id)
                    .is_none_or(|t| now.duration_since(*t) >= config.deep_interval)
            })
            .copied()
            .collect()
    }

    /// Record that a segment was scrubbed.
    pub fn record(&mut self, segment: SegmentId, kind: ScrubKind) {
        let now = Instant::now();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

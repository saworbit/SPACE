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
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            light_interval: Duration::from_secs(24 * 3600), // 1 day
            deep_interval: Duration::from_secs(7 * 24 * 3600), // 1 week
            max_segments_per_cycle: 1024,
            inter_segment_delay: Duration::from_millis(5),
        }
    }
}

/// Outcome of scrubbing a single segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScrubResult {
    /// Segment is healthy.
    Ok,
    /// Segment metadata mismatch (size, missing file).
    MetadataMismatch { expected_len: u32, actual_len: u32 },
    /// Content hash does not match stored hash (bit-rot detected).
    ContentCorrupted { segment: SegmentId },
    /// MAC verification failed.
    MacMismatch { segment: SegmentId },
    /// Segment data could not be read.
    ReadError { segment: SegmentId, error: String },
}

impl ScrubResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, ScrubResult::Ok)
    }
}

/// Aggregate report from a scrub cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScrubReport {
    pub segments_checked: usize,
    pub errors: Vec<ScrubResult>,
    pub duration: Duration,
    pub kind: ScrubKind,
}

/// Whether a scrub cycle is light (metadata-only) or deep (content verification).
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
        ];
        for case in &cases {
            assert!(!case.is_ok(), "{case:?} should not be Ok");
        }
    }

    // ── ScrubReport ─────────────────────────────────────────────────

    #[test]
    fn scrub_report_default_is_empty() {
        let report = ScrubReport::default();
        assert_eq!(report.segments_checked, 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.duration, Duration::ZERO);
        assert_eq!(report.kind, ScrubKind::Light);
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
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: ScrubConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.max_segments_per_cycle, 256);
    }
}

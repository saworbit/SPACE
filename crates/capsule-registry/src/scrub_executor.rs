//! Background scrub executor for segment integrity verification.
//!
//! RustFS's heal system demonstrated the value of a **running** background
//! integrity task  -  not just the types to describe one. SPACE already had the
//! `ScrubConfig`, `ScrubSchedule`, `ScrubResult`, and `ScrubReport` types in
//! `common::scrub`, but no executor that actually drives them.
//!
//! This module closes that gap. `ScrubExecutor` reads stored segment bytes and
//! verifies their integrity on a schedule, detecting bitrot and tampering.
//!
//! ## Verification strategy
//!
//! - **Light scrub**: checks each segment without a key. When the backend
//!   keeps a checksum of the stored bytes (`StorageBackend::verify_stored`;
//!   the data log's record frames do), light scrub verifies it, which catches
//!   media damage in any segment, encrypted or not. Otherwise it reads the
//!   bytes and confirms the length matches the `len` field in the metadata,
//!   which catches truncation and missing segments. Either way it reads every
//!   byte: "light" refers to what's checked, not to the I/O.
//!
//! - **Deep scrub**: reads the raw stored bytes and applies the strongest
//!   integrity check available for each segment:
//!   1. Encrypted segments (`encrypted == true`, key manager provided):
//!      BLAKE3-MAC verification via `encryption::verify_mac`. Detects both
//!      bitrot and tampering. A missing `integrity_tag` is a MAC failure,
//!      since the read path rejects such a segment too.
//!   2. Unencrypted segments with a `content_hash`: re-computes BLAKE3 of the
//!      stored (compressed) bytes and compares against the recorded hash.
//!   3. Segments with neither a MAC tag nor a content hash are recorded as
//!      `Ok`  -  there is no stored checksum to compare against (stable
//!      condition).
//!
//! ## Schedule recording
//!
//! A segment's scrub timestamp is advanced only for **definitive** outcomes
//! (`Ok`, `MetadataMismatch`, `ContentCorrupted`, `MacMismatch`). Transient
//! outcomes (`ReadError`, `Skipped`) leave the timestamp unchanged so the
//! segment remains due and will be retried next cycle.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use common::scrub::{
    ScheduleStore, ScrubConfig, ScrubKind, ScrubReport, ScrubResult, ScrubSchedule, ScrubState,
};
use common::traits::StorageBackend;
use common::SegmentId;
use encryption::policy::EncryptionMetadata;
use encryption::{verify_mac, KeyManager};
use tracing::{debug, info, warn};

#[cfg(test)]
use crate::dedup::{hash_content, hash_content_with_algo};
use crate::dedup::{verify_content_hash, VerifyOutcome};

/// Runs scrub cycles against a `StorageBackend`, verifying segment integrity.
///
/// Call [`ScrubExecutor::scrub_cycle`] for a single pass, or
/// [`ScrubExecutor::spawn_background`] to let it run continuously in a Tokio
/// background task.
pub struct ScrubExecutor<B: StorageBackend> {
    backend: B,
    key_manager: Option<Arc<Mutex<KeyManager>>>,
    schedule: ScrubSchedule,
    /// Where the schedule is kept between runs, if anywhere.
    store: Option<Arc<dyn ScheduleStore>>,
}

impl<B: StorageBackend> ScrubExecutor<B> {
    /// Create an executor without encryption key access.
    ///
    /// Deep scrubs verify only unencrypted segments' content hashes.
    /// MAC verification for encrypted segments is skipped.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            key_manager: None,
            schedule: ScrubSchedule::new(),
            store: None,
        }
    }

    /// Create an executor with access to the key manager for MAC verification
    /// of encrypted segments.
    pub fn with_key_manager(backend: B, key_manager: Arc<Mutex<KeyManager>>) -> Self {
        Self {
            backend,
            key_manager: Some(key_manager),
            schedule: ScrubSchedule::new(),
            store: None,
        }
    }

    /// Keep the schedule in `store`: load it now, and save each cycle's
    /// changes, so a restart only scrubs segments that are due (#134). If
    /// the schedule can't be loaded, every segment counts as due.
    pub fn with_schedule_store(mut self, store: Arc<dyn ScheduleStore>) -> Self {
        match store.load() {
            Ok(times) => self.schedule = ScrubSchedule::from_times(times),
            Err(err) => warn!(error = %err, "scrub: couldn't load the scrub schedule"),
        }
        self.store = Some(store);
        self
    }

    /// Run a single scrub cycle of the given kind and return its report.
    ///
    /// Respects `config.max_segments_per_cycle` and inserts
    /// `config.inter_segment_delay` between individual checks to avoid
    /// starving foreground I/O.
    pub async fn scrub_cycle(&mut self, config: &ScrubConfig, kind: ScrubKind) -> ScrubReport {
        let started = Instant::now();
        let mut report = ScrubReport::default();
        report.kind = kind;

        let all_ids = match self.backend.segment_ids().await {
            Ok(ids) => ids,
            Err(err) => {
                warn!(error = %err, "scrub: failed to list segment IDs");
                return report;
            }
        };

        self.schedule.retain(&all_ids);
        let due = match kind {
            ScrubKind::Light => self.schedule.due_for_light(&all_ids, config),
            ScrubKind::Deep => self.schedule.due_for_deep(&all_ids, config),
        };

        report.segments_deferred = due.len().saturating_sub(config.max_segments_per_cycle);
        let to_check: Vec<SegmentId> = due
            .into_iter()
            .take(config.max_segments_per_cycle)
            .collect();
        // Settled after the loop: on a live store, a segment deleted while
        // the cycle ran fails to read, and that isn't an error.
        let mut read_errors = Vec::new();

        for seg_id in to_check {
            let (result, seg_bytes, legacy_hash_hit) = match kind {
                ScrubKind::Light => self.light_check(seg_id).await,
                ScrubKind::Deep => self.deep_check(seg_id).await,
            };
            report.bytes_checked += seg_bytes;
            if legacy_hash_hit {
                report.legacy_hash_hits += 1;
            }

            // Bandwidth throttle: pace reads to stay within max_bytes_per_sec.
            // We compute how long the cumulative bytes *should* have taken at
            // the configured rate, then sleep only the surplus — nothing if
            // we're already behind.  This is naturally segment-size-aware:
            // a 4 MiB segment costs proportionally more than a 100-byte one.
            if let Some(max_bps) = config.max_bytes_per_sec {
                if max_bps > 0 {
                    let target =
                        Duration::from_secs_f64(report.bytes_checked as f64 / max_bps as f64);
                    let elapsed = started.elapsed();
                    if target > elapsed {
                        tokio::time::sleep(target - elapsed).await;
                    }
                }
            }

            if result.is_skipped() {
                // Transient condition  -  don't advance the schedule timestamp
                // so the segment will be retried next cycle.
                debug!(
                    segment_id = seg_id.0,
                    ?result,
                    "scrub: segment skipped (transient)"
                );
                report.segments_skipped += 1;
            } else if matches!(result, ScrubResult::ReadError { .. }) {
                read_errors.push(result);
            } else {
                report.segments_checked += 1;

                if result.should_record_schedule() {
                    // Definitive outcome  -  advance the timestamp so we don't
                    // re-check before the interval elapses.
                    self.schedule.record(seg_id, kind);
                    report.segments_recorded += 1;
                }
                // ReadError is not recorded: I/O may be transient, so the
                // segment remains due and will be retried next cycle.

                if !result.is_ok() {
                    warn!(
                        segment_id = seg_id.0,
                        ?result,
                        "scrub: integrity issue detected"
                    );
                    match &result {
                        ScrubResult::MacMismatch { .. } => report.mac_failures += 1,
                        ScrubResult::ContentCorrupted { .. } => report.content_failures += 1,
                        ScrubResult::ReadError { .. } => report.read_errors += 1,
                        ScrubResult::MetadataMismatch { .. } => report.metadata_failures += 1,
                        _ => {}
                    }
                    report.errors.push(result);
                }
            }

            if !config.inter_segment_delay.is_zero() {
                tokio::time::sleep(config.inter_segment_delay).await;
            }
        }

        if !read_errors.is_empty() {
            // If the listing fails, keep every error rather than guess.
            let present: Option<std::collections::HashSet<SegmentId>> = self
                .backend
                .segment_ids()
                .await
                .ok()
                .map(|ids| ids.into_iter().collect());
            for result in read_errors {
                let ScrubResult::ReadError { segment, .. } = &result else {
                    continue;
                };
                if present.as_ref().is_some_and(|ids| !ids.contains(segment)) {
                    debug!(
                        segment_id = segment.0,
                        "scrub: segment deleted during the cycle"
                    );
                    continue;
                }
                // Not recorded in the schedule: I/O may be transient, so the
                // segment stays due and is retried next cycle.
                warn!(
                    segment_id = segment.0,
                    ?result,
                    "scrub: segment couldn't be read"
                );
                report.segments_checked += 1;
                report.read_errors += 1;
                report.errors.push(result);
            }
        }

        if let Some(store) = &self.store {
            let changes = self.schedule.take_changes();
            if !changes.is_empty() {
                // Saving may fsync, so keep it off the async threads.
                let store = Arc::clone(store);
                let saved = tokio::task::spawn_blocking(move || store.save(&changes))
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|saved| saved);
                if let Err(err) = saved {
                    warn!(error = %err, "scrub: couldn't save the scrub schedule");
                }
            }
        }

        report.duration = started.elapsed();
        debug!(
            kind = ?kind,
            checked = report.segments_checked,
            errors = report.errors.len(),
            duration_ms = report.duration.as_millis(),
            "scrub cycle complete"
        );

        report
    }

    /// `seg_id`'s metadata. If it can't be read, a backend that checksums
    /// its own records can say whether the record is damaged, as a segment
    /// file with a damaged header is, rather than unreadable for now.
    async fn metadata_or_damage(&self, seg_id: SegmentId) -> Result<common::Segment, ScrubResult> {
        match self.backend.metadata(seg_id).await {
            Ok(meta) => Ok(meta),
            Err(err) => Err(match self.backend.verify_stored(seg_id).await {
                Ok(Some(false)) => ScrubResult::ContentCorrupted { segment: seg_id },
                _ => ScrubResult::ReadError {
                    segment: seg_id,
                    error: err.to_string(),
                },
            }),
        }
    }

    // -- Light check ----------------------------------------------------------

    /// Returns `(result, bytes_read, legacy_hash_hit)`. `legacy_hash_hit` is
    /// always `false` for light scrubs — only deep scrubs can match via the
    /// legacy bare-hash fallback.
    async fn light_check(&self, seg_id: SegmentId) -> (ScrubResult, u64, bool) {
        let meta = match self.metadata_or_damage(seg_id).await {
            Ok(m) => m,
            Err(result) => return (result, 0, false),
        };

        // A backend that checksums its own bytes, as the data log's record
        // frames do, can say more than the length can, with no key.
        match self.backend.verify_stored(seg_id).await {
            Ok(Some(true)) => return (ScrubResult::Ok, u64::from(meta.len), false),
            Ok(Some(false)) => {
                return (
                    ScrubResult::ContentCorrupted { segment: seg_id },
                    u64::from(meta.len),
                    false,
                )
            }
            Ok(None) => {}
            Err(err) => {
                return (
                    ScrubResult::ReadError {
                        segment: seg_id,
                        error: err.to_string(),
                    },
                    0,
                    false,
                )
            }
        }

        let stored = match self.backend.read(seg_id).await {
            Ok(b) => b,
            Err(err) => {
                return (
                    ScrubResult::ReadError {
                        segment: seg_id,
                        error: err.to_string(),
                    },
                    0,
                    false,
                );
            }
        };

        let bytes_read = stored.len() as u64;
        let actual_len = stored.len() as u32;
        if actual_len != meta.len {
            (
                ScrubResult::MetadataMismatch {
                    segment: seg_id,
                    expected_len: meta.len,
                    actual_len,
                },
                bytes_read,
                false,
            )
        } else {
            (ScrubResult::Ok, bytes_read, false)
        }
    }

    // -- Deep check -----------------------------------------------------------

    /// Returns `(result, bytes_read, legacy_hash_hit)`. `legacy_hash_hit` is
    /// `true` when an unencrypted segment verified only via the bare-BLAKE3
    /// fallback (pre-fix layout); the outer loop bumps
    /// `ScrubReport::legacy_hash_hits` accordingly.
    async fn deep_check(&self, seg_id: SegmentId) -> (ScrubResult, u64, bool) {
        let meta = match self.metadata_or_damage(seg_id).await {
            Ok(m) => m,
            Err(result) => return (result, 0, false),
        };

        let stored = match self.backend.read(seg_id).await {
            Ok(b) => b,
            Err(err) => {
                return (
                    ScrubResult::ReadError {
                        segment: seg_id,
                        error: err.to_string(),
                    },
                    0,
                    false,
                );
            }
        };

        let bytes_read = stored.len() as u64;

        // Length sanity first (same as light scrub).
        if stored.len() as u32 != meta.len {
            return (
                ScrubResult::MetadataMismatch {
                    segment: seg_id,
                    expected_len: meta.len,
                    actual_len: stored.len() as u32,
                },
                bytes_read,
                false,
            );
        }

        if meta.encrypted {
            // Fast synchronous checks before the CPU-intensive MAC verify.
            let Some(integrity_tag) = meta.integrity_tag else {
                // The read path refuses an encrypted segment without a tag,
                // so scrub must not pass it. Otherwise deleting the tag from
                // the unauthenticated sidecar would hide tampering (#95).
                warn!(
                    segment_id = seg_id.0,
                    "scrub: encrypted segment has no integrity tag"
                );
                return (
                    ScrubResult::MacMismatch { segment: seg_id },
                    bytes_read,
                    false,
                );
            };

            let Some(ref km) = self.key_manager else {
                // MAC exists but we can't load the key right now — transient.
                return (
                    ScrubResult::Skipped {
                        reason: "no key manager available for MAC verification".into(),
                    },
                    bytes_read,
                    false,
                );
            };

            let key_version = match meta.key_version {
                Some(v) => v,
                None => {
                    warn!(
                        segment_id = seg_id.0,
                        "scrub: encrypted segment missing key_version"
                    );
                    return (
                        ScrubResult::MacMismatch { segment: seg_id },
                        bytes_read,
                        false,
                    );
                }
            };

            let tweak = match meta.tweak_nonce {
                Some(t) => t,
                None => {
                    warn!(
                        segment_id = seg_id.0,
                        "scrub: encrypted segment missing tweak_nonce"
                    );
                    return (
                        ScrubResult::MacMismatch { segment: seg_id },
                        bytes_read,
                        false,
                    );
                }
            };

            // Extract key material while holding the lock (fast) so we don't
            // hold it across the blocking verify_mac call below.
            let key_pair = {
                let mut guard = km.lock().unwrap_or_else(|e| e.into_inner());
                match guard.get_key(key_version) {
                    Ok(kp) => (*kp.key1(), *kp.key2()),
                    Err(err) => {
                        warn!(
                            segment_id = seg_id.0,
                            error = %err,
                            "scrub: failed to retrieve key for MAC check"
                        );
                        return (
                            ScrubResult::ReadError {
                                segment: seg_id,
                                error: format!("key retrieval failed: {err}"),
                            },
                            bytes_read,
                            false,
                        );
                    }
                }
            };

            let enc_meta = EncryptionMetadata {
                encryption_version: meta.encryption_version,
                key_version: Some(key_version),
                tweak_nonce: Some(tweak),
                integrity_tag: Some(integrity_tag),
                ciphertext_len: Some(meta.len),
                ..Default::default()
            };

            // MAC verification is CPU-intensive (BLAKE3 over ciphertext).
            // Run it on a blocking thread to avoid stalling async executors.
            let result = tokio::task::spawn_blocking(move || {
                match verify_mac(&stored, &enc_meta, &key_pair.0, &key_pair.1) {
                    Ok(()) => ScrubResult::Ok,
                    Err(_) => ScrubResult::MacMismatch { segment: seg_id },
                }
            })
            .await
            .unwrap_or_else(|e| ScrubResult::ReadError {
                segment: seg_id,
                error: format!("MAC verify panicked: {e}"),
            });
            (result, bytes_read, false)
        } else {
            // Unencrypted: compare BLAKE3 content hash via `dedup::verify_content_hash`,
            // which encapsulates the post-fix algo-aware scheme and the legacy
            // bare-BLAKE3 fallback. A `LegacyMatched` outcome is integrity-clean
            // but feeds `ScrubReport::legacy_hash_hits` so operators can gauge
            // when the fallback can be retired.
            let Some(expected) = meta.content_hash.clone() else {
                return (ScrubResult::Ok, bytes_read, false);
            };
            let algo = meta.compression_algo.clone();

            // BLAKE3 over potentially large (4 MiB) segments is CPU-intensive.
            // Run it on a blocking thread to avoid stalling async executors.
            let outcome =
                tokio::task::spawn_blocking(move || verify_content_hash(&expected, &stored, &algo))
                    .await;

            match outcome {
                Ok(VerifyOutcome::Matched) => (ScrubResult::Ok, bytes_read, false),
                Ok(VerifyOutcome::LegacyMatched) => {
                    warn!(
                        segment_id = seg_id.0,
                        algo = %meta.compression_algo,
                        "scrub: segment verifies under legacy bare content hash; \
                         rewrite to upgrade to algo-domain-separated key"
                    );
                    (ScrubResult::Ok, bytes_read, true)
                }
                Ok(VerifyOutcome::Mismatched) => (
                    ScrubResult::ContentCorrupted { segment: seg_id },
                    bytes_read,
                    false,
                ),
                Err(e) => (
                    ScrubResult::ReadError {
                        segment: seg_id,
                        error: format!("hash verify panicked: {e}"),
                    },
                    bytes_read,
                    false,
                ),
            }
        }
    }
}

impl<B: StorageBackend + Clone + Send + 'static> ScrubExecutor<B> {
    /// Spawn a background Tokio task that runs scrub cycles continuously.
    ///
    /// Returns a `(JoinHandle, watch::Receiver<ScrubState>)` pair.  The
    /// receiver publishes state transitions so monitoring consumers can observe
    /// progress without polling:
    ///
    /// ```text
    /// Idle  →  Running(kind)  →  Completed { kind, errors }
    ///                              ↑ persists until the next cycle starts ↑
    /// ```
    ///
    /// `Completed.errors` counts **definitive** integrity failures only
    /// (`MetadataMismatch`, `MacMismatch`, `ContentCorrupted`).  Transient
    /// `ReadError`s are excluded so the field is suitable for alerting
    /// thresholds.
    ///
    /// The `Completed` state is **not** immediately overwritten by `Idle`; it
    /// persists until the next cycle's `Running` replaces it.  This guarantees
    /// that a consumer polling the channel at any point between cycles will
    /// observe the outcome of the most recent run, regardless of scheduling
    /// jitter.
    ///
    /// Abort the `JoinHandle` (or let the runtime shut down) to stop the task.
    pub fn spawn_background(
        backend: B,
        config: ScrubConfig,
        key_manager: Option<Arc<Mutex<KeyManager>>>,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::watch::Receiver<ScrubState>,
    ) {
        let executor = match key_manager {
            Some(km) => ScrubExecutor::with_key_manager(backend, km),
            None => ScrubExecutor::new(backend),
        };
        executor.spawn(config)
    }

    /// Run this executor in a background Tokio task, as
    /// [`ScrubExecutor::spawn_background`] does. The first cycle is deep, and
    /// checks only segments that are due: with a schedule from
    /// [`ScrubExecutor::with_schedule_store`], that's new and overdue ones.
    pub fn spawn(
        self,
        config: ScrubConfig,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::watch::Receiver<ScrubState>,
    ) {
        let (state_tx, state_rx) = tokio::sync::watch::channel(ScrubState::Idle);
        let mut executor = self;

        let handle = tokio::spawn(async move {
            let mut next_light = tokio::time::Instant::now();
            let mut next_deep = tokio::time::Instant::now();

            loop {
                let now = tokio::time::Instant::now();

                let kind = if now >= next_deep {
                    next_deep = now + config.deep_interval;
                    // Deep implies light, so reset both timers.
                    next_light = now + config.light_interval;
                    ScrubKind::Deep
                } else if now >= next_light {
                    next_light = now + config.light_interval;
                    ScrubKind::Light
                } else {
                    let until = next_light.min(next_deep);
                    tokio::time::sleep_until(until).await;
                    continue;
                };

                let _ = state_tx.send(ScrubState::Running(kind));
                let report = executor.scrub_cycle(&config, kind).await;
                // Only definitive integrity failures (not transient ReadErrors)
                // so the field is suitable for alerting thresholds.
                let _ = state_tx.send(ScrubState::Completed {
                    kind,
                    errors: report.mac_failures
                        + report.content_failures
                        + report.metadata_failures,
                });

                if report.errors.is_empty() && report.segments_checked == 0 {
                    debug!(kind = ?kind, "scrub cycle: no segments due");
                } else if report.errors.is_empty() {
                    info!(
                        kind = ?kind,
                        segments_checked = report.segments_checked,
                        duration_ms = report.duration.as_millis(),
                        "scrub cycle: all segments healthy"
                    );
                } else {
                    warn!(
                        kind = ?kind,
                        segments_checked = report.segments_checked,
                        error_count = report.errors.len(),
                        mac_failures = report.mac_failures,
                        content_failures = report.content_failures,
                        metadata_failures = report.metadata_failures,
                        read_errors = report.read_errors,
                        duration_ms = report.duration.as_millis(),
                        "scrub cycle: integrity issues detected"
                    );
                }

                // Segments recorded at different times, such as a schedule
                // loaded at startup, fall due before a whole interval has
                // passed: run the next cycle then. The slack lets one cycle
                // take segments that fall due close together.
                let wall = SystemTime::now();
                let now = tokio::time::Instant::now();
                for (timer_kind, next, interval) in [
                    (ScrubKind::Light, &mut next_light, config.light_interval),
                    (ScrubKind::Deep, &mut next_deep, config.deep_interval),
                ] {
                    if let Some(due) = executor.schedule.next_due(timer_kind, &config, wall) {
                        let wait = due.duration_since(wall).unwrap_or_default() + interval / 64;
                        *next = (*next).min(now + wait);
                    }
                }

                // More segments were due than one cycle takes: carry on with
                // them after the pause below, not a whole interval later.
                if report.segments_deferred > 0 {
                    let now = tokio::time::Instant::now();
                    match kind {
                        ScrubKind::Deep => next_deep = now,
                        ScrubKind::Light => next_light = now,
                    }
                }

                // Yield briefly to avoid spinning in three cases:
                // 1. The cycle hit max_segments_per_cycle (more work pending).
                // 2. All segments were Skipped (e.g. key manager unavailable).
                // 3. All segments hit ReadError (transient I/O).
                // Cases 2 and 3 share the same root: work was attempted but no
                // schedule timestamps were advanced, so next_deep/next_light
                // reset to `now` and would fire again immediately on a
                // zero-duration interval.
                let attempted = report.segments_checked + report.segments_skipped;
                if report.segments_deferred > 0 || (attempted > 0 && report.segments_recorded == 0)
                {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                // NOTE: we do NOT send Idle here.  Completed persists as the
                // channel's current value until the next cycle's Running
                // overwrites it, ensuring consumers can always read the
                // outcome of the most-recent run without a TOCTOU gap.
            }
        });

        (handle, state_rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::scrub::{ScrubConfig, ScrubTimes};
    use common::traits::StorageTransaction as _;
    use common::{ContentHash, Segment, SegmentId};
    use futures::future::BoxFuture;
    use std::collections::BTreeMap;
    use storage::InMemoryBackend;

    // ── FailReadBackend ──────────────────────────────────────────────────────
    //
    // A thin wrapper over `InMemoryBackend` that returns an error for any
    // `read()` call on a specified set of segment IDs.  Used to inject
    // transient I/O failures into tests without touching the real backend.

    #[derive(Clone)]
    struct FailReadBackend {
        inner: InMemoryBackend,
        fail_ids: std::collections::HashSet<SegmentId>,
    }

    impl common::traits::StorageBackend for FailReadBackend {
        type Transaction = <InMemoryBackend as common::traits::StorageBackend>::Transaction;

        fn append<'a>(
            &'a mut self,
            segment: SegmentId,
            data: &'a [u8],
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            self.inner.append(segment, data)
        }

        fn read(&self, segment: SegmentId) -> BoxFuture<'_, anyhow::Result<Vec<u8>>> {
            if self.fail_ids.contains(&segment) {
                Box::pin(
                    async move { anyhow::bail!("injected read failure for segment {}", segment.0) },
                )
            } else {
                self.inner.read(segment)
            }
        }

        fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, anyhow::Result<common::Segment>> {
            self.inner.metadata(segment)
        }

        fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, anyhow::Result<()>> {
            self.inner.delete(segment)
        }

        fn segment_ids(&self) -> BoxFuture<'_, anyhow::Result<Vec<SegmentId>>> {
            self.inner.segment_ids()
        }

        fn begin_txn(&mut self) -> BoxFuture<'_, anyhow::Result<Self::Transaction>> {
            self.inner.begin_txn()
        }
    }

    /// Counts reads, and deletes a `vanish` segment just before reading it,
    /// as a release on a live store can between listing and reading.
    #[derive(Clone)]
    struct ProbeBackend {
        inner: InMemoryBackend,
        vanish: std::collections::HashSet<SegmentId>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl common::traits::StorageBackend for ProbeBackend {
        type Transaction = <InMemoryBackend as common::traits::StorageBackend>::Transaction;

        fn append<'a>(
            &'a mut self,
            segment: SegmentId,
            data: &'a [u8],
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            self.inner.append(segment, data)
        }

        fn read(&self, segment: SegmentId) -> BoxFuture<'_, anyhow::Result<Vec<u8>>> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let vanish = self.vanish.contains(&segment);
            let mut inner = self.inner.clone();
            Box::pin(async move {
                if vanish {
                    inner.delete(segment).await?;
                }
                inner.read(segment).await
            })
        }

        fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, anyhow::Result<common::Segment>> {
            self.inner.metadata(segment)
        }

        fn delete<'a>(&'a mut self, segment: SegmentId) -> BoxFuture<'a, anyhow::Result<()>> {
            self.inner.delete(segment)
        }

        fn segment_ids(&self) -> BoxFuture<'_, anyhow::Result<Vec<SegmentId>>> {
            self.inner.segment_ids()
        }

        fn begin_txn(&mut self) -> BoxFuture<'_, anyhow::Result<Self::Transaction>> {
            self.inner.begin_txn()
        }
    }

    fn probe(inner: InMemoryBackend, vanish: &[u64]) -> ProbeBackend {
        ProbeBackend {
            inner,
            vanish: vanish.iter().map(|id| SegmentId(*id)).collect(),
            reads: Arc::default(),
        }
    }

    /// Write a segment (data + metadata) to the backend via a transaction.
    async fn write_segment(
        backend: &mut InMemoryBackend,
        id: u64,
        data: &[u8],
        content_hash: Option<ContentHash>,
        len_override: Option<u32>,
    ) {
        let seg_id = SegmentId(id);
        let seg = Segment {
            id: seg_id,
            offset: 0,
            len: len_override.unwrap_or(data.len() as u32),
            content_hash,
            encrypted: false,
            ..Default::default()
        };
        let mut txn = backend.begin_txn().await.unwrap();
        txn.append(seg_id, data).await.unwrap();
        txn.set_segment_metadata(seg_id, seg).await.unwrap();
        txn.commit().await.unwrap();
    }

    fn fast_config(kind_interval_zero: ScrubKind) -> ScrubConfig {
        match kind_interval_zero {
            ScrubKind::Light => ScrubConfig {
                light_interval: Duration::ZERO,
                deep_interval: Duration::from_secs(3600),
                max_segments_per_cycle: 64,
                inter_segment_delay: Duration::ZERO,
                max_bytes_per_sec: None,
            },
            ScrubKind::Deep => ScrubConfig {
                light_interval: Duration::from_secs(3600),
                deep_interval: Duration::ZERO,
                max_segments_per_cycle: 64,
                inter_segment_delay: Duration::ZERO,
                max_bytes_per_sec: None,
            },
        }
    }

    #[tokio::test]
    async fn light_scrub_healthy_segment() {
        let mut backend = InMemoryBackend::new();
        let data = b"hello scrub world";
        let hash = hash_content(data);
        write_segment(&mut backend, 1, data, Some(hash), None).await;

        let config = fast_config(ScrubKind::Light);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Light).await;

        assert_eq!(report.segments_checked, 1);
        assert!(
            report.errors.is_empty(),
            "healthy segment should pass light scrub"
        );
    }

    #[tokio::test]
    async fn light_scrub_detects_length_mismatch() {
        let mut backend = InMemoryBackend::new();
        let data = b"four";
        // Store 4 bytes but claim length is 8 in metadata.
        write_segment(&mut backend, 1, data, None, Some(8)).await;

        let config = fast_config(ScrubKind::Light);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Light).await;

        assert_eq!(report.errors.len(), 1);
        assert!(
            matches!(report.errors[0], ScrubResult::MetadataMismatch { .. }),
            "length mismatch should be caught in light scrub"
        );
    }

    #[tokio::test]
    async fn deep_scrub_healthy_segment_passes() {
        let mut backend = InMemoryBackend::new();
        let data = b"valid segment data for deep scrub";
        let hash = hash_content(data);
        write_segment(&mut backend, 1, data, Some(hash), None).await;

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 1);
        assert!(
            report.errors.is_empty(),
            "valid segment should pass deep scrub"
        );
    }

    /// Helper: write a segment with full control over `compression_algo`,
    /// simulating either pre-fix (bare hash + non-empty algo) or post-fix
    /// (algo-aware hash + non-empty algo) layouts.
    async fn write_segment_with_algo(
        backend: &mut InMemoryBackend,
        id: u64,
        data: &[u8],
        content_hash: ContentHash,
        compression_algo: &str,
    ) {
        let seg_id = SegmentId(id);
        let seg = Segment {
            id: seg_id,
            offset: 0,
            len: data.len() as u32,
            content_hash: Some(content_hash),
            compression_algo: compression_algo.to_string(),
            compressed: !compression_algo.is_empty() && compression_algo != "identity",
            encrypted: false,
            ..Default::default()
        };
        let mut txn = backend.begin_txn().await.unwrap();
        txn.append(seg_id, data).await.unwrap();
        txn.set_segment_metadata(seg_id, seg).await.unwrap();
        txn.commit().await.unwrap();
    }

    /// Post-fix layout: segment was written by the new pipeline. Its
    /// `content_hash` was computed with `hash_content_with_algo(data, algo)`
    /// and `compression_algo` is non-empty. Scrub must verify cleanly.
    #[tokio::test]
    async fn deep_scrub_post_upgrade_segment_passes() {
        let mut backend = InMemoryBackend::new();
        let data = b"segment written under the new dedup-key scheme";
        let hash = hash_content_with_algo(data, "identity");
        write_segment_with_algo(&mut backend, 1, data, hash, "identity").await;

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 1);
        assert!(
            report.errors.is_empty(),
            "post-upgrade segment must verify under hash_content_with_algo"
        );
    }

    /// Pre-fix layout (the regression scenario): the segment's
    /// `compression_algo` is non-empty (e.g. "identity", "lz4:1") because
    /// the old pipeline always populated it, but its `content_hash` was
    /// computed with bare `hash_content(data)`. Scrub must accept the
    /// legacy bare hash during the compatibility window — otherwise every
    /// existing on-disk segment would be falsely flagged as corrupted
    /// after upgrade.
    #[tokio::test]
    async fn deep_scrub_legacy_segment_with_bare_hash_passes() {
        let mut backend = InMemoryBackend::new();
        let data = b"segment written by the pre-fix pipeline";
        // Bare hash, exactly as the old write paths produced.
        let bare_hash = hash_content(data);
        // Segment metadata still records a non-empty algo; only the hash is
        // the legacy bare form. This is what the compatibility window targets.
        write_segment_with_algo(&mut backend, 1, data, bare_hash, "identity").await;

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 1);
        assert!(
            report.errors.is_empty(),
            "legacy bare-hash segment with non-empty algo must verify via \
             scrub's compatibility-window fallback (regression for the \
             false-corruption-after-upgrade bug)"
        );
        assert_eq!(
            report.legacy_hash_hits, 1,
            "legacy verification must be counted so operators can gauge migration"
        );
    }

    /// Genuine corruption: stored bytes differ from what produced the
    /// recorded hash, under either scheme. Scrub must still flag this as
    /// `ContentCorrupted` — the compatibility fallback must not become a
    /// general "accept anything" loophole.
    #[tokio::test]
    async fn deep_scrub_corruption_under_algo_metadata_still_detected() {
        let mut backend = InMemoryBackend::new();
        let original = b"original payload, length matters!";
        let corrupted = b"corrupted payload! length matters"; // same len
        assert_eq!(original.len(), corrupted.len());

        // Record an algo-aware hash of `original`, but store `corrupted`.
        let hash = hash_content_with_algo(original, "lz4:1");
        write_segment_with_algo(&mut backend, 1, corrupted, hash, "lz4:1").await;

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 1);
        assert_eq!(
            report.errors.len(),
            1,
            "neither algo-aware nor bare hash matches corrupted bytes; \
             the compatibility fallback must not mask real bitrot"
        );
        assert!(matches!(
            report.errors[0],
            ScrubResult::ContentCorrupted { .. }
        ));
    }

    #[tokio::test]
    async fn deep_scrub_detects_content_corruption() {
        let mut backend = InMemoryBackend::new();
        let original = b"original content";
        let corrupted = b"corrupted conten"; // same length, different bytes

        // Record hash of original, but store corrupted bytes.
        let hash = hash_content(original);
        write_segment(&mut backend, 1, corrupted, Some(hash), None).await;

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 1);
        assert_eq!(report.errors.len(), 1, "bitrot should be detected");
        assert!(
            matches!(report.errors[0], ScrubResult::ContentCorrupted { .. }),
            "unexpected error variant: {:?}",
            report.errors[0]
        );
    }

    #[tokio::test]
    async fn scrub_respects_max_segments_per_cycle() {
        let mut backend = InMemoryBackend::new();
        for i in 0u64..10 {
            let data = format!("segment {i}");
            let hash = hash_content(data.as_bytes());
            write_segment(&mut backend, i, data.as_bytes(), Some(hash), None).await;
        }

        let config = ScrubConfig {
            light_interval: Duration::ZERO,
            deep_interval: Duration::from_secs(3600),
            max_segments_per_cycle: 3,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Light).await;

        assert_eq!(
            report.segments_checked, 3,
            "should respect max_segments_per_cycle"
        );
    }

    #[tokio::test]
    async fn schedule_prevents_rescrub_before_interval() {
        let mut backend = InMemoryBackend::new();
        let data = b"some data";
        let hash = hash_content(data);
        write_segment(&mut backend, 1, data, Some(hash), None).await;

        // Set a long interval so it won't expire within the test.
        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::from_secs(7200),
            max_segments_per_cycle: 64,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };
        let mut executor = ScrubExecutor::new(backend);

        // First cycle: segment is due (never scrubbed).
        let first = executor.scrub_cycle(&config, ScrubKind::Light).await;
        assert_eq!(first.segments_checked, 1);

        // Second cycle immediately after: interval hasn't elapsed, nothing due.
        let second = executor.scrub_cycle(&config, ScrubKind::Light).await;
        assert_eq!(
            second.segments_checked, 0,
            "should not re-scrub before interval elapses"
        );
    }

    #[tokio::test]
    async fn empty_backend_produces_empty_report() {
        let backend = InMemoryBackend::new();
        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(report.segments_checked, 0);
        assert_eq!(report.segments_skipped, 0);
        assert!(report.errors.is_empty());
    }

    // -- Skip semantics -------------------------------------------------------

    /// Write a segment whose metadata marks it as encrypted with an integrity
    /// tag but give the executor no key manager  -  the segment cannot be
    /// verified and should be skipped (not recorded in the schedule).
    async fn write_encrypted_segment_no_data(backend: &mut InMemoryBackend, id: u64, data: &[u8]) {
        let seg_id = SegmentId(id);
        let seg = Segment {
            id: seg_id,
            offset: 0,
            len: data.len() as u32,
            encrypted: true,
            integrity_tag: Some([0xab; 16]), // pretend a MAC was stored
            key_version: Some(1),
            tweak_nonce: Some([0u8; 16]),
            encryption_version: Some(1),
            ..Default::default()
        };
        let mut txn = backend.begin_txn().await.unwrap();
        txn.append(seg_id, data).await.unwrap();
        txn.set_segment_metadata(seg_id, seg).await.unwrap();
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn encrypted_segment_without_key_manager_is_skipped() {
        let mut backend = InMemoryBackend::new();
        write_encrypted_segment_no_data(&mut backend, 1, b"ciphertext bytes").await;

        let config = fast_config(ScrubKind::Deep);
        // Executor created without a key manager.
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(
            report.segments_checked, 0,
            "segment should not be counted as checked"
        );
        assert_eq!(
            report.segments_skipped, 1,
            "segment should be counted as skipped"
        );
        assert!(report.errors.is_empty(), "skipped is not an error");
    }

    #[tokio::test]
    async fn skipped_segment_remains_due_next_cycle() {
        let mut backend = InMemoryBackend::new();
        write_encrypted_segment_no_data(&mut backend, 1, b"ciphertext bytes").await;

        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::ZERO,
            max_segments_per_cycle: 64,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };
        let mut executor = ScrubExecutor::new(backend);

        // First cycle: segment is skipped (no key manager).
        let first = executor.scrub_cycle(&config, ScrubKind::Deep).await;
        assert_eq!(first.segments_skipped, 1);

        // Second cycle immediately after: segment is STILL due because the
        // schedule timestamp was never advanced.
        let second = executor.scrub_cycle(&config, ScrubKind::Deep).await;
        assert_eq!(
            second.segments_skipped, 1,
            "skipped segment must remain due for retry"
        );
        assert_eq!(second.segments_checked, 0);
    }

    #[tokio::test]
    async fn encrypted_segment_without_integrity_tag_is_a_mac_failure() {
        // A missing tag is a definitive failure, not a pass and not a
        // transient skip, so it consumes its scheduling slot.
        let mut backend = InMemoryBackend::new();
        let seg_id = SegmentId(1);
        let data = b"old ciphertext without mac";
        let seg = Segment {
            id: seg_id,
            offset: 0,
            len: data.len() as u32,
            encrypted: true,
            integrity_tag: None, // no MAC  -  pre-MAC segment
            key_version: Some(1),
            ..Default::default()
        };
        let mut txn = backend.begin_txn().await.unwrap();
        txn.append(seg_id, data).await.unwrap();
        txn.set_segment_metadata(seg_id, seg).await.unwrap();
        txn.commit().await.unwrap();

        let config = fast_config(ScrubKind::Deep);
        let mut executor = ScrubExecutor::new(backend);
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;

        assert_eq!(
            report.segments_checked, 1,
            "should be counted as checked (stable condition)"
        );
        assert_eq!(report.segments_skipped, 0);
        assert_eq!(report.mac_failures, 1);
        assert_eq!(report.errors.len(), 1);

        // Because it was recorded, a second cycle should find nothing due.
        let config_no_retry = ScrubConfig {
            deep_interval: Duration::from_secs(3600),
            ..config
        };
        let second = executor
            .scrub_cycle(&config_no_retry, ScrubKind::Deep)
            .await;
        assert_eq!(
            second.segments_checked, 0,
            "a definitive result must not be re-checked immediately"
        );
    }

    // -- spawn_background state channel ------------------------------------------

    /// Verify that `spawn_background` emits `Running` then `Completed` and
    /// that `Completed` persists as the channel value (not immediately
    /// overwritten by `Idle`).
    #[tokio::test]
    async fn spawn_background_state_channel_observability() {
        let mut backend = InMemoryBackend::new();
        let data = b"state channel test segment";
        let hash = hash_content(data);
        write_segment(&mut backend, 1, data, Some(hash), None).await;

        // Both intervals set far in the future so that exactly one cycle fires
        // immediately (next_deep == now at startup → Deep is chosen first),
        // then the background task sleeps for 7200 s.  This prevents the
        // InMemoryBackend's synchronous-disguised-as-async reads from starving
        // the current-thread executor with a zero-interval spin loop.
        let config = ScrubConfig {
            light_interval: Duration::from_secs(7200),
            deep_interval: Duration::from_secs(7200),
            max_segments_per_cycle: 64,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };

        let (handle, mut state_rx) = ScrubExecutor::spawn_background(backend, config, None);

        // Wait until we observe Completed.  The loop handles the case where the
        // background task is so fast that Running and Completed have already
        // fired by the time changed() is first called — watch::changed() will
        // fire on the *next* value change from the last-seen mark, so looping
        // on changed() + borrow() is the correct pattern.
        let (observed_kind, observed_errors) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    state_rx.changed().await.expect("background task alive");
                    if let ScrubState::Completed { kind, errors } = *state_rx.borrow() {
                        return (kind, errors);
                    }
                }
            })
            .await
            .expect("should observe ScrubState::Completed within 5 seconds");

        // The background loop checks next_deep before next_light, and both
        // start at Instant::now(), so the first (and only) cycle is always Deep.
        assert_eq!(observed_kind, ScrubKind::Deep);
        assert_eq!(
            observed_errors, 0,
            "healthy segment should report zero integrity errors in Completed"
        );

        // Abort the background task and verify Completed persists — it must
        // not have been immediately overwritten by Idle.
        handle.abort();
        let _ = handle.await;

        assert!(
            matches!(*state_rx.borrow(), ScrubState::Completed { .. }),
            "Completed state must persist between cycles, not be overwritten by Idle"
        );
    }

    #[tokio::test]
    async fn a_segment_deleted_during_a_cycle_is_not_an_error() {
        let mut inner = InMemoryBackend::new();
        for id in 1..=2 {
            let data = format!("segment {id}").into_bytes();
            write_segment(&mut inner, id, &data, Some(hash_content(&data)), None).await;
        }
        let mut executor = ScrubExecutor::new(probe(inner, &[2]));
        let report = executor
            .scrub_cycle(&fast_config(ScrubKind::Deep), ScrubKind::Deep)
            .await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.read_errors, 0);
        assert_eq!(report.segments_checked, 1);
    }

    #[tokio::test]
    async fn the_schedule_forgets_deleted_segments() {
        let mut backend = InMemoryBackend::new();
        for id in 1..=2 {
            write_segment(&mut backend, id, b"data", None, None).await;
        }
        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            ..fast_config(ScrubKind::Deep)
        };
        let mut executor = ScrubExecutor::new(backend.clone());
        executor.scrub_cycle(&config, ScrubKind::Light).await;
        assert_eq!(executor.schedule.last_light.len(), 2);

        backend.delete(SegmentId(2)).await.unwrap();
        executor.scrub_cycle(&config, ScrubKind::Light).await;
        assert_eq!(
            executor
                .schedule
                .last_light
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [SegmentId(1)]
        );
    }

    #[tokio::test]
    async fn a_capped_cycle_carries_on_without_waiting_an_interval() {
        let mut inner = InMemoryBackend::new();
        for id in 1..=3 {
            write_segment(&mut inner, id, b"data", None, None).await;
        }
        let config = ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::from_secs(3600),
            max_segments_per_cycle: 1,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };

        let mut executor = ScrubExecutor::new(inner.clone());
        let report = executor.scrub_cycle(&config, ScrubKind::Deep).await;
        assert_eq!(report.segments_deferred, 2);

        // The background loop takes the rest a second apart, not an hour.
        let backend = probe(inner, &[]);
        let reads = Arc::clone(&backend.reads);
        let (handle, _state) = ScrubExecutor::spawn_background(backend, config, None);
        tokio::time::timeout(Duration::from_secs(5), async {
            while reads.load(std::sync::atomic::Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the deferred segments weren't scrubbed within 5 seconds");
        handle.abort();
    }

    /// Verify that `Completed.errors` counts only definitive failures
    /// (`MetadataMismatch`, `MacMismatch`, `ContentCorrupted`) and excludes
    /// transient `ReadError`s.
    ///
    /// Setup:
    /// - Segment 1: corrupted bytes (hash mismatch) → `ContentCorrupted` (counted)
    /// - Segment 2: read injected to fail via `FailReadBackend` → `ReadError` (excluded)
    ///
    /// Expected: `Completed.errors == 1`, not 2.
    #[tokio::test]
    async fn spawn_background_completed_errors_excludes_read_errors() {
        let mut inner = InMemoryBackend::new();

        // Segment 1: ContentCorrupted → definitive failure (must be counted).
        let original = b"original content data";
        let hash = hash_content(original);
        let corrupted = b"corrupted content dat"; // same length, different bytes
        write_segment(&mut inner, 1, corrupted, Some(hash), None).await;

        // Segment 2: stored correctly so metadata succeeds, but read will be
        // intercepted → ReadError (must NOT be counted in Completed.errors).
        let good_data = b"good data for segment two";
        let good_hash = hash_content(good_data);
        write_segment(&mut inner, 2, good_data, Some(good_hash), None).await;

        let backend = FailReadBackend {
            inner,
            fail_ids: [SegmentId(2)].into_iter().collect(),
        };

        // Long re-cycle intervals: exactly one deep cycle fires at startup
        // (next_deep == now), then the task sleeps for 7200 s.  This avoids
        // the ZERO-interval spin that would let a second cycle overwrite
        // Completed in the watch channel before the test can observe it.
        let config = ScrubConfig {
            light_interval: Duration::from_secs(7200),
            deep_interval: Duration::from_secs(7200),
            max_segments_per_cycle: 64,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        };

        let (handle, mut state_rx) = ScrubExecutor::spawn_background(backend, config, None);

        let (_, errors) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                state_rx.changed().await.expect("background task alive");
                if let ScrubState::Completed { kind, errors } = *state_rx.borrow() {
                    return (kind, errors);
                }
            }
        })
        .await
        .expect("should observe ScrubState::Completed within 5 seconds");

        assert_eq!(
            errors, 1,
            "ContentCorrupted is definitive (counted); ReadError is transient (excluded)"
        );

        handle.abort();
        let _ = handle.await;
    }

    // ── Schedule kept between runs (#134) ───────────────────────────────────

    #[derive(Default)]
    struct MemStore(Mutex<BTreeMap<SegmentId, ScrubTimes>>);

    impl ScheduleStore for MemStore {
        fn load(&self) -> anyhow::Result<Vec<(SegmentId, ScrubTimes)>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect())
        }

        fn save(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> anyhow::Result<()> {
            let mut saved = self.0.lock().unwrap();
            for (segment, times) in changes {
                match times {
                    Some(t) => saved.insert(*segment, *t),
                    None => saved.remove(segment),
                };
            }
            Ok(())
        }
    }

    fn hourly() -> ScrubConfig {
        ScrubConfig {
            light_interval: Duration::from_secs(3600),
            deep_interval: Duration::from_secs(7200),
            max_segments_per_cycle: 64,
            inter_segment_delay: Duration::ZERO,
            max_bytes_per_sec: None,
        }
    }

    #[tokio::test]
    async fn a_restarted_executor_scrubs_only_what_is_due() {
        let mut backend = InMemoryBackend::new();
        for id in 1..=3u64 {
            let data = format!("segment {id}");
            let hash = hash_content(data.as_bytes());
            write_segment(&mut backend, id, data.as_bytes(), Some(hash), None).await;
        }
        let store = Arc::new(MemStore::default());

        let mut first = ScrubExecutor::new(backend.clone()).with_schedule_store(store.clone());
        let report = first.scrub_cycle(&hourly(), ScrubKind::Deep).await;
        assert_eq!(report.segments_checked, 3);
        assert_eq!(store.0.lock().unwrap().len(), 3);
        drop(first);

        // A new executor, as after a restart, picks up where the last left off.
        let mut second = ScrubExecutor::new(backend.clone()).with_schedule_store(store.clone());
        let deep = second.scrub_cycle(&hourly(), ScrubKind::Deep).await;
        let light = second.scrub_cycle(&hourly(), ScrubKind::Light).await;
        assert_eq!((deep.segments_checked, light.segments_checked), (0, 0));

        // A segment written since is due at once.
        write_segment(
            &mut backend,
            4,
            b"segment 4",
            Some(hash_content(b"segment 4")),
            None,
        )
        .await;
        let report = second.scrub_cycle(&hourly(), ScrubKind::Deep).await;
        assert_eq!(report.segments_checked, 1);
        assert_eq!(store.0.lock().unwrap().len(), 4);

        // A deleted segment leaves the store.
        backend.delete(SegmentId(2)).await.unwrap();
        second.scrub_cycle(&hourly(), ScrubKind::Light).await;
        assert!(!store.0.lock().unwrap().contains_key(&SegmentId(2)));
    }

    #[tokio::test]
    async fn the_background_loop_runs_when_a_loaded_segment_falls_due() {
        let mut backend = InMemoryBackend::new();
        write_segment(
            &mut backend,
            1,
            b"due soon",
            Some(hash_content(b"due soon")),
            None,
        )
        .await;
        let config = ScrubConfig {
            // The loop's slack is a 64th of this: 100 ms.
            deep_interval: Duration::from_millis(6400),
            ..hourly()
        };
        let now = SystemTime::now();
        // Due for a deep scrub 400 ms from now.
        let last_deep = now - Duration::from_millis(6000);
        let store = Arc::new(MemStore::default());
        store.0.lock().unwrap().insert(
            SegmentId(1),
            ScrubTimes {
                light: Some(now),
                deep: Some(last_deep),
            },
        );

        let started = Instant::now();
        let (task, _state) = ScrubExecutor::new(backend)
            .with_schedule_store(store.clone())
            .spawn(config);
        // The first cycle finds nothing due. Waiting a whole interval after
        // it would take 6.4 s.
        loop {
            let saved = store.0.lock().unwrap()[&SegmentId(1)].deep;
            if saved.is_some_and(|t| t > last_deep) {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the segment wasn't scrubbed when it fell due"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        task.abort();
    }
}

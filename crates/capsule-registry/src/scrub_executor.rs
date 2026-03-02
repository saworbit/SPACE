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
//! - **Light scrub**: reads each segment's bytes and confirms the length
//!   matches the `len` field in its metadata. This catches truncation and
//!   missing segments. Note: `StorageBackend` has no stat-only method, so a
//!   full read is required; the "light" in light scrub refers to the
//!   verification being length-only, not to reduced I/O.
//!
//! - **Deep scrub**: reads the raw stored bytes and applies the strongest
//!   integrity check available for each segment:
//!   1. Encrypted segments (`encrypted == true`, `integrity_tag` set, key
//!      manager provided): BLAKE3-MAC verification via
//!      `encryption::verify_mac`. Detects both bitrot and tampering.
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
use std::time::{Duration, Instant};

use common::scrub::{ScrubConfig, ScrubKind, ScrubReport, ScrubResult, ScrubSchedule, ScrubState};
use common::traits::StorageBackend;
use common::SegmentId;
use encryption::policy::EncryptionMetadata;
use encryption::{verify_mac, KeyManager};
use tracing::{debug, info, warn};

use crate::dedup::hash_content;

/// Runs scrub cycles against a `StorageBackend`, verifying segment integrity.
///
/// Call [`ScrubExecutor::scrub_cycle`] for a single pass, or
/// [`ScrubExecutor::spawn_background`] to let it run continuously in a Tokio
/// background task.
pub struct ScrubExecutor<B: StorageBackend> {
    backend: B,
    key_manager: Option<Arc<Mutex<KeyManager>>>,
    schedule: ScrubSchedule,
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
        }
    }

    /// Create an executor with access to the key manager for MAC verification
    /// of encrypted segments.
    pub fn with_key_manager(backend: B, key_manager: Arc<Mutex<KeyManager>>) -> Self {
        Self {
            backend,
            key_manager: Some(key_manager),
            schedule: ScrubSchedule::new(),
        }
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

        let due = match kind {
            ScrubKind::Light => self.schedule.due_for_light(&all_ids, config),
            ScrubKind::Deep => self.schedule.due_for_deep(&all_ids, config),
        };

        let to_check: Vec<SegmentId> = due
            .into_iter()
            .take(config.max_segments_per_cycle)
            .collect();

        for seg_id in to_check {
            let result = match kind {
                ScrubKind::Light => self.light_check(seg_id).await,
                ScrubKind::Deep => self.deep_check(seg_id).await,
            };

            if result.is_skipped() {
                // Transient condition  -  don't advance the schedule timestamp
                // so the segment will be retried next cycle.
                debug!(
                    segment_id = seg_id.0,
                    ?result,
                    "scrub: segment skipped (transient)"
                );
                report.segments_skipped += 1;
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

    // -- Light check ----------------------------------------------------------

    async fn light_check(&self, seg_id: SegmentId) -> ScrubResult {
        let meta = match self.backend.metadata(seg_id).await {
            Ok(m) => m,
            Err(err) => {
                return ScrubResult::ReadError {
                    segment: seg_id,
                    error: err.to_string(),
                };
            }
        };

        let stored = match self.backend.read(seg_id).await {
            Ok(b) => b,
            Err(err) => {
                return ScrubResult::ReadError {
                    segment: seg_id,
                    error: err.to_string(),
                };
            }
        };

        let actual_len = stored.len() as u32;
        if actual_len != meta.len {
            ScrubResult::MetadataMismatch {
                expected_len: meta.len,
                actual_len,
            }
        } else {
            ScrubResult::Ok
        }
    }

    // -- Deep check -----------------------------------------------------------

    async fn deep_check(&self, seg_id: SegmentId) -> ScrubResult {
        let meta = match self.backend.metadata(seg_id).await {
            Ok(m) => m,
            Err(err) => {
                return ScrubResult::ReadError {
                    segment: seg_id,
                    error: err.to_string(),
                };
            }
        };

        let stored = match self.backend.read(seg_id).await {
            Ok(b) => b,
            Err(err) => {
                return ScrubResult::ReadError {
                    segment: seg_id,
                    error: err.to_string(),
                };
            }
        };

        // Length sanity first (same as light scrub).
        if stored.len() as u32 != meta.len {
            return ScrubResult::MetadataMismatch {
                expected_len: meta.len,
                actual_len: stored.len() as u32,
            };
        }

        if meta.encrypted {
            // Fast synchronous checks before the CPU-intensive MAC verify.
            let Some(integrity_tag) = meta.integrity_tag else {
                // No MAC was ever stored — the segment was encrypted before
                // MAC support was added. Stable condition: record as Ok.
                debug!(
                    segment_id = seg_id.0,
                    "scrub: encrypted segment has no integrity tag; nothing to verify"
                );
                return ScrubResult::Ok;
            };

            let Some(ref km) = self.key_manager else {
                // MAC exists but we can't load the key right now — transient.
                return ScrubResult::Skipped {
                    reason: "no key manager available for MAC verification".into(),
                };
            };

            let key_version = match meta.key_version {
                Some(v) => v,
                None => {
                    warn!(
                        segment_id = seg_id.0,
                        "scrub: encrypted segment missing key_version"
                    );
                    return ScrubResult::MacMismatch { segment: seg_id };
                }
            };

            let tweak = match meta.tweak_nonce {
                Some(t) => t,
                None => {
                    warn!(
                        segment_id = seg_id.0,
                        "scrub: encrypted segment missing tweak_nonce"
                    );
                    return ScrubResult::MacMismatch { segment: seg_id };
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
                        return ScrubResult::ReadError {
                            segment: seg_id,
                            error: format!("key retrieval failed: {err}"),
                        };
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
            tokio::task::spawn_blocking(move || {
                match verify_mac(&stored, &enc_meta, &key_pair.0, &key_pair.1) {
                    Ok(()) => ScrubResult::Ok,
                    Err(_) => ScrubResult::MacMismatch { segment: seg_id },
                }
            })
            .await
            .unwrap_or_else(|e| ScrubResult::ReadError {
                segment: seg_id,
                error: format!("MAC verify panicked: {e}"),
            })
        } else {
            // Unencrypted: compare BLAKE3 content hash.
            let Some(expected) = meta.content_hash.clone() else {
                return ScrubResult::Ok;
            };

            // BLAKE3 over potentially large (4 MiB) segments is CPU-intensive.
            // Run it on a blocking thread to avoid stalling async executors.
            tokio::task::spawn_blocking(move || {
                let actual = hash_content(&stored);
                if actual.as_str() == expected.as_str() {
                    ScrubResult::Ok
                } else {
                    ScrubResult::ContentCorrupted { segment: seg_id }
                }
            })
            .await
            .unwrap_or_else(|e| ScrubResult::ReadError {
                segment: seg_id,
                error: format!("hash verify panicked: {e}"),
            })
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
        let (state_tx, state_rx) = tokio::sync::watch::channel(ScrubState::Idle);

        let handle = tokio::spawn(async move {
            let mut executor = match key_manager {
                Some(km) => ScrubExecutor::with_key_manager(backend, km),
                None => ScrubExecutor::new(backend),
            };

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

                if report.errors.is_empty() {
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

                // Yield briefly to avoid spinning in three cases:
                // 1. The cycle hit max_segments_per_cycle (more work pending).
                // 2. All segments were Skipped (e.g. key manager unavailable).
                // 3. All segments hit ReadError (transient I/O).
                // Cases 2 and 3 share the same root: work was attempted but no
                // schedule timestamps were advanced, so next_deep/next_light
                // reset to `now` and would fire again immediately on a
                // zero-duration interval.
                let attempted = report.segments_checked + report.segments_skipped;
                if report.segments_checked >= config.max_segments_per_cycle
                    || (attempted > 0 && report.segments_recorded == 0)
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
    use common::scrub::ScrubConfig;
    use common::traits::{StorageBackend as _, StorageTransaction as _};
    use common::{ContentHash, Segment, SegmentId};
    use futures::future::BoxFuture;
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
            },
            ScrubKind::Deep => ScrubConfig {
                light_interval: Duration::from_secs(3600),
                deep_interval: Duration::ZERO,
                max_segments_per_cycle: 64,
                inter_segment_delay: Duration::ZERO,
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
    async fn encrypted_segment_without_integrity_tag_is_ok_not_skipped() {
        // No MAC was ever stored  -  there is nothing to verify. This is a
        // stable (not transient) condition, so it should be recorded as Ok
        // and consume its scheduling slot.
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
        assert!(report.errors.is_empty());

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
            "stable-Ok segment must not be re-checked immediately"
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
}

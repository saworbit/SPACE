use crate::compression::{compress_segment, decompress_lz4, decompress_zstd};
use crate::dedup::{hash_content, DedupStats};
use crate::{gc::GarbageCollector, CapsuleRegistry};
use anyhow::Result;
use common::*;
use nvram_sim::NvramLog;
#[cfg(feature = "pipeline_async")]
use nvram_sim::NvramTransaction;
use std::borrow::Cow;
use std::collections::HashMap;

// Phase 3: Encryption imports
use encryption::{
    compute_mac, decrypt_segment, derive_tweak_from_hash, encrypt_segment, verify_mac,
    EncryptionMetadata, KeyManager,
};
use std::sync::{Arc, Mutex}; // NEW: For interior mutability
#[cfg(feature = "pipeline_async")]
use std::time::{Duration, Instant};

#[cfg(feature = "pipeline_async")]
use futures::future::join_all;
#[cfg(feature = "pipeline_async")]
use tokio::runtime::Builder as RuntimeBuilder;
#[cfg(feature = "pipeline_async")]
use tokio::sync::{mpsc, Semaphore};
#[cfg(feature = "pipeline_async")]
use tokio::task::{spawn_blocking, JoinHandle};
#[cfg(feature = "pipeline_async")]
use tracing::{debug, info, trace};

#[cfg(feature = "pipeline_async")]
#[derive(Clone)]
pub struct PipelineConfig {
    pub max_concurrency: usize,
    pub memory_limit_per_task: usize,
    pub use_transactions: bool,
}

#[cfg(feature = "pipeline_async")]
impl Default for PipelineConfig {
    fn default() -> Self {
        let cpu_parallelism = std::cmp::max(1, num_cpus::get() / 2);
        Self {
            max_concurrency: cpu_parallelism,
            memory_limit_per_task: 1usize << 30, // 1 GiB
            use_transactions: false,
        }
    }
}

#[cfg(feature = "pipeline_async")]
fn prepare_segment(
    index: usize,
    chunk: Vec<u8>,
    policy: Policy,
    key_manager: Option<Arc<Mutex<KeyManager>>>,
) -> Result<SegmentPrepared> {
    let started = Instant::now();
    let (compressed_data, comp_result) = compress_segment(&chunk, &policy.compression)?;
    let content_hash = hash_content(compressed_data.as_ref());

    let encryption_enabled = policy.encryption.is_enabled() && key_manager.is_some();
    let mut encryption_meta = None;

    let final_data = if encryption_enabled {
        let km = key_manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Key manager unavailable for encryption"))?;
        let mut km = km.lock().unwrap();

        let key_version = km.current_version();
        let key_pair = km.get_key(key_version)?;

        let tweak = derive_tweak_from_hash(content_hash.as_str().as_bytes());
        let (ciphertext, mut enc_meta) =
            encrypt_segment(compressed_data.as_ref(), key_pair, key_version, tweak)?;

        let mac_tag = compute_mac(&ciphertext, &enc_meta, key_pair.key1(), key_pair.key2())?;
        enc_meta.set_integrity_tag(mac_tag);
        encryption_meta = Some(enc_meta);
        ciphertext
    } else {
        match compressed_data {
            Cow::Borrowed(data) => data.to_vec(),
            Cow::Owned(vec) => vec,
        }
    };

    Ok(SegmentPrepared {
        index,
        content_hash,
        final_data,
        comp_result,
        encryption_meta,
        prepared_at: Instant::now(),
        preparation_time: started.elapsed(),
    })
}

#[cfg(feature = "pipeline_async")]
struct SegmentPrepared {
    index: usize,
    content_hash: ContentHash,
    final_data: Vec<u8>,
    comp_result: crate::compression::CompressionResult,
    encryption_meta: Option<EncryptionMetadata>,
    prepared_at: Instant,
    preparation_time: Duration,
}

#[cfg(feature = "pipeline_async")]
enum WriteDisposition {
    NewSegment,
    ReusedPersistent,
    ReusedStaged,
}

pub struct WritePipeline {
    registry: CapsuleRegistry,
    nvram: NvramLog,
    key_manager: Option<Arc<Mutex<KeyManager>>>, // CHANGED: Wrapped in Arc<Mutex<>>
    #[cfg(feature = "pipeline_async")]
    config: PipelineConfig,
}

impl WritePipeline {
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        // Try to initialize key manager from environment
        let key_manager = KeyManager::from_env()
            .ok()
            .map(|km| Arc::new(Mutex::new(km))); // CHANGED: Wrap in Arc<Mutex<>>

        if key_manager.is_some() {
            println!("🔐 Encryption enabled (key manager initialized)");
        }

        let pipeline = Self {
            registry,
            nvram,
            key_manager,
            #[cfg(feature = "pipeline_async")]
            config: PipelineConfig::default(),
        };

        if let Err(err) = pipeline.reconcile_refcounts() {
            eprintln!("⚠️  Failed to reconcile segment refcounts: {:?}", err);
        }

        pipeline
    }

    /// Create pipeline with explicit key manager (for testing)
    pub fn with_key_manager(
        registry: CapsuleRegistry,
        nvram: NvramLog,
        key_manager: KeyManager,
    ) -> Self {
        Self {
            registry,
            nvram,
            key_manager: Some(Arc::new(Mutex::new(key_manager))), // CHANGED: Wrap in Arc<Mutex<>>
            #[cfg(feature = "pipeline_async")]
            config: PipelineConfig::default(),
        }
    }

    #[cfg(feature = "pipeline_async")]
    pub fn with_config(mut self, config: PipelineConfig) -> Self {
        self.config = config;
        self
    }

    fn reconcile_refcounts(&self) -> Result<()> {
        let mut counts: HashMap<SegmentId, u32> = HashMap::new();

        for capsule_id in self.registry.list_capsules() {
            if let Ok(capsule) = self.registry.lookup(capsule_id) {
                for seg_id in capsule.segments {
                    counts.entry(seg_id).and_modify(|c| *c += 1).or_insert(1);
                }
            }
        }

        let segments = self.nvram.list_segments()?;
        for mut segment in segments {
            let expected = *counts.get(&segment.id).unwrap_or(&0);
            if segment.ref_count != expected {
                segment.ref_count = expected;
                segment.deduplicated = expected > 1;
                self.nvram
                    .update_segment_metadata(segment.id, segment.clone())?;
            }
        }

        // Sweep any orphaned segments with ref_count == 0.
        let gc = GarbageCollector::new(&self.registry, &self.nvram);
        gc.sweep()?;

        Ok(())
    }

    pub fn delete_capsule(&self, capsule_id: CapsuleId) -> Result<()> {
        let capsule = self.registry.delete_capsule(capsule_id)?;

        for seg_id in capsule.segments {
            let segment = self.nvram.decrement_refcount(seg_id)?;

            if segment.ref_count == 0 {
                if let Some(ref hash) = segment.content_hash {
                    self.registry.deregister_content(hash, seg_id)?;
                }
                self.nvram.remove_segment(seg_id)?;
            }
        }

        Ok(())
    }

    pub fn garbage_collect(&self) -> Result<usize> {
        let gc = GarbageCollector::new(&self.registry, &self.nvram);
        gc.sweep()
    }

    /// Write data with compression and return the capsule ID
    pub fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        self.write_capsule_with_policy(data, &Policy::default())
    }

    /// Write data with explicit policy (including encryption)
    #[cfg(not(feature = "pipeline_async"))]
    pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        // Pre-allocate capsule ID but don't persist yet
        let capsule_id = CapsuleId::new();

        // Track stats
        let mut segment_ids = Vec::new();
        let mut total_compressed_size = 0u64;
        let mut total_original_size = 0u64;
        let mut dedup_stats = DedupStats::new();

        // Check if encryption is enabled
        let encryption_enabled = policy.encryption.is_enabled() && self.key_manager.is_some();

        // Split into segments, compress, deduplicate, and encrypt
        for chunk in data.chunks(SEGMENT_SIZE) {
            total_original_size += chunk.len() as u64;

            // Step 1: Compress the segment based on policy
            let (compressed_data, comp_result) = compress_segment(chunk, &policy.compression)?;
            total_compressed_size += comp_result.compressed_size as u64;

            // Step 2: Hash the compressed data for deduplication
            let content_hash = hash_content(compressed_data.as_ref());

            // Step 3: Encrypt if enabled (before dedup check for deterministic encryption)
            let mut encryption_meta = None;
            let final_data = if encryption_enabled {
                let km = self.key_manager.as_ref().unwrap();
                let mut km = km.lock().unwrap(); // CHANGED: Lock the mutex
                let key_version = km.current_version();
                let key_pair = km.get_key(key_version)?;

                // Derive deterministic tweak from content hash
                let tweak = derive_tweak_from_hash(content_hash.as_str().as_bytes());

                // Encrypt segment
                let (ciphertext, mut enc_meta) =
                    encrypt_segment(compressed_data.as_ref(), key_pair, key_version, tweak)?;

                // Compute MAC over ciphertext + metadata
                let mac_tag =
                    compute_mac(&ciphertext, &enc_meta, key_pair.key1(), key_pair.key2())?;

                enc_meta.set_integrity_tag(mac_tag);

                encryption_meta = Some(enc_meta);
                Cow::Owned(ciphertext)
            } else {
                compressed_data
            };

            // Step 4: Check if this content already exists (if dedup enabled)
            let (seg_id, was_deduped) = if policy.dedupe {
                if let Some(existing_seg_id) = self.registry.lookup_content(&content_hash) {
                    // Content exists! Reuse the segment
                    let updated_segment = self.nvram.increment_refcount(existing_seg_id)?;
                    let saved_bytes = updated_segment.len as u64;

                    dedup_stats.add_segment(saved_bytes, true);

                    println!(
                        "  ♻️  Dedup hit: Reusing segment {} (saved {} bytes, ref_count={})",
                        existing_seg_id.0, saved_bytes, updated_segment.ref_count
                    );

                    (existing_seg_id, true)
                } else {
                    // New content - allocate and write
                    let new_seg_id = self.registry.alloc_segment();

                    // Write to NVRAM
                    let mut segment = self.nvram.append(new_seg_id, final_data.as_ref())?;

                    // Update segment metadata - compression
                    segment.compressed = comp_result.compressed;
                    segment.compression_algo = comp_result.algorithm.clone();
                    segment.content_hash = Some(content_hash.clone());
                    segment.ref_count = 1;
                    segment.deduplicated = false;

                    // Update segment metadata - encryption
                    if let Some(ref enc_meta) = encryption_meta {
                        segment.encrypted = true;
                        segment.encryption_version = enc_meta.encryption_version;
                        segment.key_version = enc_meta.key_version;
                        segment.tweak_nonce = enc_meta.tweak_nonce;
                        segment.integrity_tag = enc_meta.integrity_tag;
                    }

                    // Save updated metadata back to NVRAM
                    self.nvram.update_segment_metadata(new_seg_id, segment)?;

                    // Register in content store
                    self.registry.register_content(content_hash, new_seg_id)?;

                    dedup_stats.add_segment(final_data.len() as u64, false);

                    (new_seg_id, false)
                }
            } else {
                // Dedup disabled - always write new segment
                let new_seg_id = self.registry.alloc_segment();

                let mut segment = self.nvram.append(new_seg_id, final_data.as_ref())?;
                segment.compressed = comp_result.compressed;
                segment.compression_algo = comp_result.algorithm.clone();
                segment.ref_count = 1;
                segment.deduplicated = false;

                // Update segment metadata - encryption
                if let Some(ref enc_meta) = encryption_meta {
                    segment.encrypted = true;
                    segment.encryption_version = enc_meta.encryption_version;
                    segment.key_version = enc_meta.key_version;
                    segment.tweak_nonce = enc_meta.tweak_nonce;
                    segment.integrity_tag = enc_meta.integrity_tag;
                }

                // Save updated metadata back to NVRAM
                self.nvram.update_segment_metadata(new_seg_id, segment)?;

                dedup_stats.add_segment(final_data.len() as u64, false);

                (new_seg_id, false)
            };

            segment_ids.push(seg_id);

            // Log stats
            if !was_deduped {
                if encryption_enabled {
                    println!(
                        "  🔐 Segment {}: encrypted with key v{}",
                        seg_id.0,
                        encryption_meta.as_ref().unwrap().key_version.unwrap()
                    );
                }
                if comp_result.compressed {
                    println!(
                        "  🗜️  Segment {}: {:.2}x compression ({} -> {} bytes, {})",
                        seg_id.0,
                        comp_result.ratio(),
                        comp_result.original_size,
                        comp_result.compressed_size,
                        comp_result.algorithm
                    );
                }
            }
        }

        // Only create capsule metadata after all segments are durable
        self.registry
            .create_capsule_with_segments(capsule_id, data.len() as u64, segment_ids)?;

        // Update dedup stats on capsule
        if dedup_stats.bytes_saved > 0 {
            self.registry
                .add_deduped_bytes(capsule_id, dedup_stats.bytes_saved)?;
        }

        // Print summary stats
        let compression_ratio = if total_compressed_size > 0 {
            total_original_size as f32 / total_compressed_size as f32
        } else {
            1.0
        };

        let encryption_status = if encryption_enabled {
            " 🔐 encrypted"
        } else {
            ""
        };

        println!(
            "✅ Capsule {}: {:.2}x compression, {} dedup hits ({} bytes saved){}",
            capsule_id.as_uuid(),
            compression_ratio,
            dedup_stats.deduped_segments,
            dedup_stats.bytes_saved,
            encryption_status
        );

        Ok(capsule_id)
    }

    #[cfg(feature = "pipeline_async")]
    pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(self.write_capsule_with_policy_async(data, policy)),
            Err(_) => {
                let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
                runtime.block_on(self.write_capsule_with_policy_async(data, policy))
            }
        }
    }

    #[cfg(feature = "pipeline_async")]
    pub async fn write_capsule_with_policy_async(
        &self,
        data: &[u8],
        policy: &Policy,
    ) -> Result<CapsuleId> {
        let pipeline_start = Instant::now();
        let capsule_id = CapsuleId::new();

        let encryption_enabled = policy.encryption.is_enabled() && self.key_manager.is_some();
        let total_segments = (data.len() + SEGMENT_SIZE - 1) / SEGMENT_SIZE;

        if total_segments == 0 {
            self.registry
                .create_capsule_with_segments(capsule_id, 0, Vec::new())?;
            info!(
                capsule = %capsule_id.as_uuid(),
                "async write pipeline completed (empty capsule)"
            );
            return Ok(capsule_id);
        }

        info!(
            capsule = %capsule_id.as_uuid(),
            segments = total_segments,
            "async write pipeline start"
        );

        let mut transaction = self.nvram.begin_transaction()?;
        let mut staged_content: HashMap<ContentHash, SegmentId> = HashMap::new();
        let mut dedupe_increments: Vec<SegmentId> = Vec::new();
        let mut pending_registrations: Vec<(ContentHash, SegmentId)> = Vec::new();

        let (tx, mut rx) = mpsc::channel(std::cmp::max(1, total_segments));
        let semaphore = Arc::new(Semaphore::new(std::cmp::max(
            1,
            self.config.max_concurrency,
        )));

        let mut handles: Vec<JoinHandle<Result<()>>> = Vec::with_capacity(total_segments);

        for (index, chunk) in data.chunks(SEGMENT_SIZE).enumerate() {
            let permit = semaphore.clone().acquire_owned().await?;
            let tx = tx.clone();
            let policy_clone = policy.clone();
            let key_manager = self.key_manager.clone();

            if chunk.len() > self.config.memory_limit_per_task {
                anyhow::bail!(
                    "Segment {} exceeds configured per-task memory limit ({} bytes > {} bytes)",
                    index,
                    chunk.len(),
                    self.config.memory_limit_per_task
                );
            }

            let chunk_vec = chunk.to_vec();

            handles.push(tokio::spawn(async move {
                let _permit = permit;

                let mut prepared = spawn_blocking(move || {
                    prepare_segment(index, chunk_vec, policy_clone, key_manager)
                })
                .await??;

                prepared.prepared_at = Instant::now();
                trace!(
                    segment = index,
                    preparation_us = prepared.preparation_time.as_micros() as u64,
                    "segment prepared"
                );

                tx.send(prepared)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to enqueue segment {}: {}", index, e))?;
                Ok(())
            }));
        }
        drop(tx);

        let mut ordered: Vec<Option<SegmentPrepared>> = Vec::with_capacity(total_segments);
        ordered.resize_with(total_segments, || None);
        let mut next_index = 0usize;

        let mut segment_ids = Vec::with_capacity(total_segments);
        let mut total_compressed_size = 0u64;
        let mut total_original_size = 0u64;
        let mut dedup_stats = DedupStats::new();

        let mut preparation_total = Duration::ZERO;
        let mut preparation_max = Duration::ZERO;
        let mut coordination_total = Duration::ZERO;
        let mut coordination_max = Duration::ZERO;
        let mut commit_total = Duration::ZERO;
        let mut prepared_segments = 0usize;
        let mut new_segment_count = 0usize;
        let mut staged_reuse_count = 0usize;

        let mut commit_error: Option<anyhow::Error> = None;

        'outer: while let Some(prepared) = rx.recv().await {
            let idx = prepared.index;
            preparation_total += prepared.preparation_time;
            if prepared.preparation_time > preparation_max {
                preparation_max = prepared.preparation_time;
            }
            prepared_segments += 1;

            ordered[idx] = Some(prepared);

            while next_index < total_segments {
                let Some(next_prepared) = ordered[next_index].take() else {
                    break;
                };

                total_original_size += next_prepared.comp_result.original_size as u64;
                total_compressed_size += next_prepared.comp_result.compressed_size as u64;

                let coordination_start = Instant::now();
                let coordination_delay = coordination_start - next_prepared.prepared_at;
                coordination_total += coordination_delay;
                if coordination_delay > coordination_max {
                    coordination_max = coordination_delay;
                }

                let commit_start = Instant::now();
                match self.commit_segment(
                    next_prepared,
                    policy,
                    encryption_enabled,
                    &mut transaction,
                    &mut staged_content,
                ) {
                    Ok((seg_id, disposition, bytes_tracked, registered_hash)) => {
                        let commit_duration = commit_start.elapsed();
                        commit_total += commit_duration;

                        let disposition_label = match disposition {
                            WriteDisposition::NewSegment => {
                                if let Some(hash) = registered_hash {
                                    pending_registrations.push((hash, seg_id));
                                }
                                new_segment_count += 1;
                                dedup_stats.add_segment(bytes_tracked, false);
                                "new"
                            }
                            WriteDisposition::ReusedPersistent => {
                                dedupe_increments.push(seg_id);
                                dedup_stats.add_segment(bytes_tracked, true);
                                "reuse_persistent"
                            }
                            WriteDisposition::ReusedStaged => {
                                staged_reuse_count += 1;
                                dedup_stats.add_segment(bytes_tracked, true);
                                "reuse_staged"
                            }
                        };

                        trace!(
                            segment = seg_id.0,
                            disposition = disposition_label,
                            coordination_us = coordination_delay.as_micros() as u64,
                            commit_us = commit_duration.as_micros() as u64,
                            "segment committed"
                        );

                        segment_ids.push(seg_id);
                        next_index += 1;
                    }
                    Err(err) => {
                        commit_error = Some(err);
                        break 'outer;
                    }
                }
            }
        }

        drop(rx);

        let join_results = join_all(handles).await;
        for handle_res in join_results {
            match handle_res {
                Ok(Ok(())) => {}
                Ok(Err(task_err)) => {
                    if commit_error.is_none() {
                        commit_error = Some(task_err);
                    }
                }
                Err(join_err) => {
                    if commit_error.is_none() {
                        commit_error = Some(anyhow::Error::from(join_err));
                    }
                }
            }
        }

        if commit_error.is_none() && next_index != total_segments {
            commit_error = Some(anyhow::anyhow!(
                "async pipeline exited early: processed {} of {} segments",
                next_index,
                total_segments
            ));
        }

        if let Some(err) = commit_error {
            transaction.rollback()?;
            for seg_id in dedupe_increments.iter().rev() {
                let _ = self.nvram.decrement_refcount(*seg_id)?;
            }
            info!(
                capsule = %capsule_id.as_uuid(),
                "async write pipeline aborted; staged work rolled back"
            );
            return Err(err);
        }

        if let Err(err) = transaction.commit() {
            for seg_id in dedupe_increments.iter().rev() {
                let _ = self.nvram.decrement_refcount(*seg_id)?;
            }
            return Err(err);
        }

        let mut registered = Vec::new();
        for (hash, seg_id) in &pending_registrations {
            if let Err(err) = self.registry.register_content(hash.clone(), *seg_id) {
                for (registered_hash, registered_seg_id) in &registered {
                    let _ = self
                        .registry
                        .deregister_content(registered_hash, *registered_seg_id)?;
                }
                for (_, seg_id) in &pending_registrations {
                    let _ = self.nvram.remove_segment(*seg_id)?;
                }
                for seg_id in dedupe_increments.iter().rev() {
                    let _ = self.nvram.decrement_refcount(*seg_id)?;
                }
                info!(
                    capsule = %capsule_id.as_uuid(),
                    "async write pipeline aborted during content registration"
                );
                return Err(err);
            }
            registered.push((hash.clone(), *seg_id));
        }

        if let Err(err) = self.registry.create_capsule_with_segments(
            capsule_id,
            data.len() as u64,
            segment_ids.clone(),
        ) {
            for (hash, seg_id) in &pending_registrations {
                let _ = self.registry.deregister_content(hash, *seg_id)?;
                let _ = self.nvram.remove_segment(*seg_id)?;
            }
            for seg_id in dedupe_increments.iter().rev() {
                let _ = self.nvram.decrement_refcount(*seg_id)?;
            }
            return Err(err);
        }

        if dedup_stats.bytes_saved > 0 {
            self.registry
                .add_deduped_bytes(capsule_id, dedup_stats.bytes_saved)?;
        }

        let compression_ratio = if total_compressed_size > 0 {
            total_original_size as f32 / total_compressed_size as f32
        } else {
            1.0
        };

        let pipeline_elapsed = pipeline_start.elapsed();
        let prep_avg_us = if prepared_segments > 0 {
            preparation_total.as_micros() as f64 / prepared_segments as f64
        } else {
            0.0
        };
        let coord_avg_us = if prepared_segments > 0 {
            coordination_total.as_micros() as f64 / prepared_segments as f64
        } else {
            0.0
        };

        info!(
            capsule = %capsule_id.as_uuid(),
            segments = total_segments,
            new_segments = new_segment_count,
            dedupe_persistent = dedupe_increments.len(),
            dedupe_staged = staged_reuse_count,
            preparation_avg_us = prep_avg_us,
            preparation_max_us = preparation_max.as_micros(),
            coordination_avg_us = coord_avg_us,
            coordination_max_us = coordination_max.as_micros(),
            commit_ms = commit_total.as_secs_f64() * 1_000.0,
            total_ms = pipeline_elapsed.as_secs_f64() * 1_000.0,
            "async write pipeline complete"
        );

        let encryption_status = if encryption_enabled { " encrypted" } else { "" };
        println!(
            "Capsule {}: {:.2}x compression, {} dedup hits ({} bytes saved){}",
            capsule_id.as_uuid(),
            compression_ratio,
            dedup_stats.deduped_segments,
            dedup_stats.bytes_saved,
            encryption_status
        );

        Ok(capsule_id)
    }
    #[cfg(feature = "pipeline_async")]
    fn commit_segment(
        &self,
        prepared: SegmentPrepared,
        policy: &Policy,
        encryption_enabled: bool,
        transaction: &mut NvramTransaction,
        staged_content: &mut HashMap<ContentHash, SegmentId>,
    ) -> Result<(SegmentId, WriteDisposition, u64, Option<ContentHash>)> {
        let SegmentPrepared {
            index: _,
            content_hash,
            final_data,
            comp_result,
            encryption_meta,
            ..
        } = prepared;

        if policy.dedupe {
            if let Some(&staged_seg_id) = staged_content.get(&content_hash) {
                let pending_segment =
                    transaction.pending_segment(staged_seg_id).ok_or_else(|| {
                        anyhow::anyhow!("pending segment {:?} not found", staged_seg_id)
                    })?;
                let saved_bytes = pending_segment.len as u64;
                transaction.with_segment_mut(staged_seg_id, |segment| {
                    segment.ref_count = segment.ref_count.saturating_add(1);
                    segment.deduplicated = segment.ref_count > 1;
                })?;

                trace!(
                    segment = staged_seg_id.0,
                    saved_bytes,
                    "dedupe hit using staged segment"
                );

                return Ok((
                    staged_seg_id,
                    WriteDisposition::ReusedStaged,
                    saved_bytes,
                    None,
                ));
            }

            if let Some(existing_seg_id) = self.registry.lookup_content(&content_hash) {
                let segment = self.nvram.increment_refcount(existing_seg_id)?;
                let saved_bytes = segment.len as u64;

                trace!(
                    segment = existing_seg_id.0,
                    saved_bytes,
                    "dedupe hit using committed segment"
                );

                return Ok((
                    existing_seg_id,
                    WriteDisposition::ReusedPersistent,
                    saved_bytes,
                    None,
                ));
            }
        }

        let seg_id = self.registry.alloc_segment();
        let data_len = final_data.len() as u64;
        let mut segment = transaction.append_segment(seg_id, final_data.as_ref())?;

        segment.compressed = comp_result.compressed;
        segment.compression_algo = comp_result.algorithm.clone();
        segment.ref_count = 1;
        segment.deduplicated = false;

        let registered_hash = if policy.dedupe {
            segment.content_hash = Some(content_hash.clone());
            staged_content.insert(content_hash.clone(), seg_id);
            Some(content_hash)
        } else {
            segment.content_hash = None;
            None
        };

        if let Some(ref enc_meta) = encryption_meta {
            segment.encrypted = true;
            segment.encryption_version = enc_meta.encryption_version;
            segment.key_version = enc_meta.key_version;
            segment.tweak_nonce = enc_meta.tweak_nonce;
            segment.integrity_tag = enc_meta.integrity_tag;
        }

        transaction.set_segment_metadata(seg_id, segment)?;

        if encryption_enabled && encryption_meta.is_some() {
            debug!(segment = seg_id.0, "segment encrypted");
        }
        if comp_result.compressed {
            debug!(
                segment = seg_id.0,
                algorithm = %comp_result.algorithm,
                ratio = comp_result.ratio(),
                "segment compressed"
            );
        }

        Ok((
            seg_id,
            WriteDisposition::NewSegment,
            data_len,
            registered_hash,
        ))
    }
    /// Read entire capsule contents (with decryption and decompression)
    pub fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.registry.lookup(id)?;

        let mut result = Vec::with_capacity(capsule.size as usize);

        for seg_id in &capsule.segments {
            // Read raw data from NVRAM
            let raw_data = self.nvram.read(*seg_id)?;

            // Get segment metadata to check if encrypted
            let segment = self.nvram.get_segment_metadata(*seg_id)?;

            // Step 1: Decrypt if encrypted
            let decrypted_data = if segment.encrypted {
                // Verify we have a key manager
                let km = self.key_manager.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Cannot decrypt: key manager not initialized")
                })?;

                let mut km = km.lock().unwrap(); // CHANGED: Lock the mutex

                // Get the key version used for this segment
                let key_version = segment
                    .key_version
                    .ok_or_else(|| anyhow::anyhow!("Missing key version in encrypted segment"))?;

                let key_pair = km.get_key(key_version)?;

                // Build encryption metadata from segment
                let enc_meta = EncryptionMetadata {
                    encryption_version: segment.encryption_version,
                    key_version: segment.key_version,
                    tweak_nonce: segment.tweak_nonce,
                    integrity_tag: segment.integrity_tag,
                    ciphertext_len: Some(raw_data.len() as u32),
                };

                // Verify MAC first
                verify_mac(&raw_data, &enc_meta, key_pair.key1(), key_pair.key2())?;

                // Decrypt
                decrypt_segment(&raw_data, key_pair, &enc_meta)?
            } else {
                raw_data
            };

            // Step 2: Decompress based on policy
            let data = match capsule.policy.compression {
                CompressionPolicy::None => decrypted_data,
                CompressionPolicy::LZ4 { .. } => {
                    match decompress_lz4(&decrypted_data) {
                        Ok(decompressed) => decompressed,
                        Err(_) => decrypted_data, // Wasn't compressed
                    }
                }
                CompressionPolicy::Zstd { .. } => {
                    match decompress_zstd(&decrypted_data) {
                        Ok(decompressed) => decompressed,
                        Err(_) => decrypted_data, // Wasn't compressed
                    }
                }
            };

            result.extend_from_slice(&data);
        }

        Ok(result)
    }

    /// Read a range within a capsule (for block/file semantics)
    pub fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let capsule = self.registry.lookup(id)?;

        if offset + len as u64 > capsule.size {
            anyhow::bail!("Read beyond capsule boundary");
        }

        // Simple implementation - read full capsule then slice
        // TODO Phase 2.3: Optimize to only read relevant segments
        let full_data = self.read_capsule(id)?;
        Ok(full_data[offset as usize..(offset as usize + len)].to_vec())
    }
}

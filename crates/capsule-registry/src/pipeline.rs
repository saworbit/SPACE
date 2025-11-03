use crate::compression::{compress_segment, decompress_lz4, decompress_zstd};
use crate::dedup::{hash_content, DedupStats};
use crate::{gc::GarbageCollector, CapsuleRegistry};
use anyhow::Result;
use common::*;
use nvram_sim::NvramLog;
use std::borrow::Cow;
use std::collections::HashMap;

// Phase 3: Encryption imports
use encryption::{
    compute_mac, decrypt_segment, derive_tweak_from_hash, encrypt_segment, verify_mac,
    EncryptionMetadata, KeyManager,
};
use std::sync::{Arc, Mutex}; // NEW: For interior mutability

pub struct WritePipeline {
    registry: CapsuleRegistry,
    nvram: NvramLog,
    key_manager: Option<Arc<Mutex<KeyManager>>>, // CHANGED: Wrapped in Arc<Mutex<>>
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
        }
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

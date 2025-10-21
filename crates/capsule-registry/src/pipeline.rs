use common::*;
use crate::CapsuleRegistry;
use crate::compression::{compress_segment, decompress_lz4, decompress_zstd};
use crate::dedup::{hash_content, DedupStats};
use nvram_sim::NvramLog;
use anyhow::Result;

pub struct WritePipeline {
    registry: CapsuleRegistry,
    nvram: NvramLog,
}

impl WritePipeline {
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        Self { registry, nvram }
    }

    /// Write data with compression and return the capsule ID
    pub fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        self.write_capsule_with_policy(data, &Policy::default())
    }

    /// Write data with explicit policy (including deduplication)
    pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        // Pre-allocate capsule ID but don't persist yet
        let capsule_id = CapsuleId::new();
        
        // Track stats
        let mut segment_ids = Vec::new();
        let mut total_compressed_size = 0u64;
        let mut total_original_size = 0u64;
        let mut dedup_stats = DedupStats::new();
        
        // Split into segments, compress, and deduplicate
        for chunk in data.chunks(SEGMENT_SIZE) {
            total_original_size += chunk.len() as u64;
            
            // Step 1: Compress the segment based on policy
            let (compressed_data, comp_result) = compress_segment(chunk, &policy.compression)?;
            total_compressed_size += comp_result.compressed_size as u64;
            
            // Step 2: Hash the compressed data for deduplication
            let content_hash = hash_content(&compressed_data);
            
            // Step 3: Check if this content already exists (if dedup enabled)
            let (seg_id, was_deduped) = if policy.dedupe {
                if let Some(existing_seg_id) = self.registry.lookup_content(&content_hash) {
                    // Content exists! Reuse the segment
                    dedup_stats.add_segment(compressed_data.len() as u64, true);
                    
                    println!("  ♻️  Dedup hit: Reusing segment {} (saved {} bytes)", 
                        existing_seg_id.0, compressed_data.len());
                    
                    (existing_seg_id, true)
                } else {
                    // New content - allocate and write
                    let new_seg_id = self.registry.alloc_segment();
                    
                    // Write to NVRAM
                    let mut segment = self.nvram.append(new_seg_id, &compressed_data)?;
                    
                    // Update segment metadata
                    segment.compressed = comp_result.compressed;
                    segment.compression_algo = comp_result.algorithm.clone();
                    segment.content_hash = Some(content_hash.clone());
                    segment.ref_count = 1;
                    segment.deduplicated = false;
                    
                    // Register in content store
                    self.registry.register_content(content_hash, new_seg_id)?;
                    
                    dedup_stats.add_segment(compressed_data.len() as u64, false);
                    
                    (new_seg_id, false)
                }
            } else {
                // Dedup disabled - always write new segment
                let new_seg_id = self.registry.alloc_segment();
                
                let mut segment = self.nvram.append(new_seg_id, &compressed_data)?;
                segment.compressed = comp_result.compressed;
                segment.compression_algo = comp_result.algorithm.clone();
                segment.ref_count = 1;
                segment.deduplicated = false;
                
                dedup_stats.add_segment(compressed_data.len() as u64, false);
                
                (new_seg_id, false)
            };
            
            segment_ids.push(seg_id);
            
            // Log compression stats
            if comp_result.compressed && !was_deduped {
                println!("  🗜️  Segment {}: {:.2}x compression ({} -> {} bytes, {})", 
                    seg_id.0, comp_result.ratio(), 
                    comp_result.original_size, comp_result.compressed_size,
                    comp_result.algorithm);
            }
        }
        
        // Only create capsule metadata after all segments are durable
        self.registry.create_capsule_with_segments(capsule_id, data.len() as u64, segment_ids)?;
        
        // Update dedup stats on capsule
        if dedup_stats.bytes_saved > 0 {
            self.registry.add_deduped_bytes(capsule_id, dedup_stats.bytes_saved)?;
        }
        
        // Print summary stats
        let compression_ratio = if total_compressed_size > 0 {
            total_original_size as f32 / total_compressed_size as f32
        } else {
            1.0
        };
        
        println!("✅ Capsule {}: {:.2}x compression, {} dedup hits ({} bytes saved)", 
            capsule_id.as_uuid(), compression_ratio, 
            dedup_stats.deduped_segments, dedup_stats.bytes_saved);
        
        Ok(capsule_id)
    }

    /// Read entire capsule contents (with decompression)
    pub fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.registry.lookup(id)?;
        
        let mut result = Vec::with_capacity(capsule.size as usize);
        
        for seg_id in &capsule.segments {
            let compressed_data = self.nvram.read(*seg_id)?;
            
            // Decompress based on policy
            let data = match capsule.policy.compression {
                CompressionPolicy::None => compressed_data,
                CompressionPolicy::LZ4 { .. } => {
                    match decompress_lz4(&compressed_data) {
                        Ok(decompressed) => decompressed,
                        Err(_) => compressed_data, // Wasn't compressed
                    }
                }
                CompressionPolicy::Zstd { .. } => {
                    match decompress_zstd(&compressed_data) {
                        Ok(decompressed) => decompressed,
                        Err(_) => compressed_data, // Wasn't compressed
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
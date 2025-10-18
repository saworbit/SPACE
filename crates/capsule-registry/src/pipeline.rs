use common::*;
use crate::CapsuleRegistry;
use crate::compression::{compress_segment, decompress_lz4, decompress_zstd};
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

    /// Write data with explicit policy
    pub fn write_capsule_with_policy(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId> {
        // Pre-allocate capsule ID but don't persist yet
        let capsule_id = CapsuleId::new();
        
        // Collect segments with compression stats
        let mut segment_ids = Vec::new();
        let mut total_compressed_size = 0u64;
        let mut total_original_size = 0u64;
        
        // Split into segments and compress
        for chunk in data.chunks(SEGMENT_SIZE) {
            let seg_id = self.registry.alloc_segment();
            
            // Compress the segment based on policy
            let (compressed_data, comp_result) = compress_segment(chunk, &policy.compression)?;
            
            total_original_size += comp_result.original_size as u64;
            total_compressed_size += comp_result.compressed_size as u64;
            
            // Append compressed data to NVRAM log (can fail)
            let mut segment = self.nvram.append(seg_id, &compressed_data)?;
            
            // Update segment metadata with compression info
            segment.compressed = comp_result.compressed;
            segment.compression_algo = comp_result.algorithm.clone();
            segment.deduplicated = false; // Phase 2.2 will add dedupe
            segment.access_count = 0;
            
            segment_ids.push(seg_id);
            
            // Log compression stats (optional, can remove for production)
            if comp_result.compressed {
                println!("  Segment {}: {:.2}x compression ({} -> {} bytes, {})", 
                    seg_id.0, comp_result.ratio(), 
                    comp_result.original_size, comp_result.compressed_size,
                    comp_result.algorithm);
            }
        }
        
        // Only create capsule metadata after all segments are durable
        self.registry.create_capsule_with_segments(capsule_id, data.len() as u64, segment_ids)?;
        
        // Print summary stats
        let compression_ratio = if total_compressed_size > 0 {
            total_original_size as f32 / total_compressed_size as f32
        } else {
            1.0
        };
        
        println!("✅ Capsule {}: {:.2}x overall compression ({} -> {} bytes)", 
            capsule_id.as_uuid(), compression_ratio, 
            total_original_size, total_compressed_size);
        
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
        // TODO Phase 2.1: Optimize to only read relevant segments
        let full_data = self.read_capsule(id)?;
        Ok(full_data[offset as usize..(offset as usize + len)].to_vec())
    }
}
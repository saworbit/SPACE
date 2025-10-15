use common::*;
use crate::CapsuleRegistry;
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

/// Write data and return the capsule ID
    pub fn write_capsule(&self, data: &[u8]) -> Result<CapsuleId> {
        // Pre-allocate capsule ID but don't persist yet
        let capsule_id = CapsuleId::new();
        
        // Collect segments first
        let mut segment_ids = Vec::new();
        
        // Split into segments and write to NVRAM
        for chunk in data.chunks(SEGMENT_SIZE) {
            let seg_id = self.registry.alloc_segment();
            
            // Append to NVRAM log (can fail)
            self.nvram.append(seg_id, chunk)?;
            
            segment_ids.push(seg_id);
        }
        
        // Only create capsule metadata after all segments are durable
        self.registry.create_capsule_with_segments(capsule_id, data.len() as u64, segment_ids)?;
        
        Ok(capsule_id)
    }

    /// Read entire capsule contents
    pub fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>> {
        let capsule = self.registry.lookup(id)?;
        
        let mut result = Vec::with_capacity(capsule.size as usize);
        
        for seg_id in &capsule.segments {
            let data = self.nvram.read(*seg_id)?;
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

        // Simple implementation - optimize later
        let full_data = self.read_capsule(id)?;
        Ok(full_data[offset as usize..(offset as usize + len)].to_vec())
    }
}
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
        // Create capsule metadata
        let capsule_id = self.registry.create_capsule(data.len() as u64)?;
        
        // Split into segments
        for chunk in data.chunks(SEGMENT_SIZE) {
            let seg_id = self.registry.alloc_segment();
            
            // Append to NVRAM log
            self.nvram.append(seg_id, chunk)?;
            
            // Link segment to capsule
            self.registry.add_segment(capsule_id, seg_id)?;
        }
        
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
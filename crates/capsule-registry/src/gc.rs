use crate::CapsuleRegistry;
use anyhow::{anyhow, Result};
use common::Segment;
use nvram_sim::NvramLog;

/// Simple reference-count based garbage collector.
///
/// Scans the NVRAM metadata for segments whose `ref_count` has dropped to zero
/// and removes both the metadata entry and the corresponding content-store
/// record from the registry.
pub struct GarbageCollector<'a> {
    registry: &'a CapsuleRegistry,
    nvram: &'a NvramLog,
}

impl<'a> GarbageCollector<'a> {
    pub fn new(registry: &'a CapsuleRegistry, nvram: &'a NvramLog) -> Self {
        Self { registry, nvram }
    }

    /// Run a sweep pass and return the number of reclaimed segments.
    pub fn sweep(&self) -> Result<usize> {
        let segments = self.nvram.list_segments()?;
        let mut reclaimed = 0usize;

        for segment in segments {
            if segment.ref_count == 0 {
                self.reclaim_segment(segment)?;
                reclaimed += 1;
            }
        }

        Ok(reclaimed)
    }

    fn reclaim_segment(&self, segment: Segment) -> Result<()> {
        if let Some(ref hash) = segment.content_hash {
            self.registry.deregister_content(hash, segment.id)?;
        }

        // Remove the metadata entry; the on-disk bytes remain until compaction.
        if self.nvram.remove_segment(segment.id)?.is_none() {
            return Err(anyhow!("Segment {:?} vanished during GC", segment.id));
        }

        Ok(())
    }
}

use common::*;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use anyhow::Result;

pub struct CapsuleRegistry {
    capsules: Arc<RwLock<HashMap<CapsuleId, Capsule>>>,
    next_segment_id: Arc<RwLock<u64>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self {
            capsules: Arc::new(RwLock::new(HashMap::new())),
            next_segment_id: Arc::new(RwLock::new(0)),
        }
    }

    pub fn create_capsule(&self, size: u64) -> Result<CapsuleId> {
        let id = CapsuleId::new();
        let mut capsules = self.capsules.write().unwrap();
        
        if capsules.contains_key(&id) {
            anyhow::bail!("Capsule collision (extremely unlikely)");
        }

        let capsule = Capsule {
            id,
            size,
            segments: Vec::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        };

        capsules.insert(id, capsule);
        Ok(id)
    }

    pub fn lookup(&self, id: CapsuleId) -> Result<Capsule> {
        self.capsules.read().unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))
    }

    pub fn alloc_segment(&self) -> SegmentId {
        let mut next = self.next_segment_id.write().unwrap();
        let id = *next;
        *next += 1;
        SegmentId(id)
    }

    pub fn add_segment(&self, capsule_id: CapsuleId, seg_id: SegmentId) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();
        let capsule = capsules.get_mut(&capsule_id)
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))?;
        capsule.segments.push(seg_id);
        Ok(())
    }

    pub fn list_capsules(&self) -> Vec<CapsuleId> {
        self.capsules.read().unwrap()
            .keys()
            .copied()
            .collect()
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        self.capsules.write().unwrap()
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))
    }
}
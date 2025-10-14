use common::*;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::path::Path;
use std::fs;
use anyhow::Result;
use serde::{Serialize, Deserialize};

pub mod pipeline;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryState {
    capsules: HashMap<CapsuleId, Capsule>,
    next_segment_id: u64,
}

pub struct CapsuleRegistry {
    capsules: Arc<RwLock<HashMap<CapsuleId, Capsule>>>,
    next_segment_id: Arc<RwLock<u64>>,
    metadata_path: String,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.metadata").expect("Failed to open registry")
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let metadata_path = path.as_ref().to_string_lossy().to_string();
        
        // Try to load existing state
        let (capsules, next_segment_id) = if Path::new(&metadata_path).exists() {
            let data = fs::read_to_string(&metadata_path)?;
            let state: RegistryState = serde_json::from_str(&data)?;
            (state.capsules, state.next_segment_id)
        } else {
            (HashMap::new(), 0)
        };

        Ok(Self {
            capsules: Arc::new(RwLock::new(capsules)),
            next_segment_id: Arc::new(RwLock::new(next_segment_id)),
            metadata_path,
        })
    }

    pub fn save(&self) -> Result<()> {
        let state = RegistryState {
            capsules: self.capsules.read().unwrap().clone(),
            next_segment_id: *self.next_segment_id.read().unwrap(),
        };
        
        let json = serde_json::to_string_pretty(&state)?;
        fs::write(&self.metadata_path, json)?;
        Ok(())
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
        drop(capsules); // Release lock before saving
        self.save()?;   // Persist after every change
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
        drop(capsules);
        self.save()?;  // Persist after every change
        Ok(())
    }

    pub fn list_capsules(&self) -> Vec<CapsuleId> {
        self.capsules.read().unwrap()
            .keys()
            .copied()
            .collect()
    }

    pub fn delete_capsule(&self, id: CapsuleId) -> Result<Capsule> {
        let capsule = self.capsules.write().unwrap()
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("Capsule not found"))?;
        self.save()?;
        Ok(capsule)
    }
}

impl Default for CapsuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
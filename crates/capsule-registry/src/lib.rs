use common::*;
use common::Policy;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::path::Path;
use std::fs;
use anyhow::Result;
use serde::{Serialize, Deserialize};

pub mod pipeline;
pub mod compression;
pub mod dedup;  // NEW

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryState {
    capsules: HashMap<CapsuleId, Capsule>,
    next_segment_id: u64,
    // Phase 2.2: Content-addressed storage for deduplication
    #[serde(default)]
    content_store: HashMap<ContentHash, SegmentId>,
}

pub struct CapsuleRegistry {
    capsules: Arc<RwLock<HashMap<CapsuleId, Capsule>>>,
    next_segment_id: Arc<RwLock<u64>>,
    metadata_path: String,
    // Phase 2.2: Content store for deduplication
    content_store: Arc<RwLock<HashMap<ContentHash, SegmentId>>>,
}

impl CapsuleRegistry {
    pub fn new() -> Self {
        Self::open("space.metadata").expect("Failed to open registry")
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let metadata_path = path.as_ref().to_string_lossy().to_string();
        
        // Try to load existing state
        let (capsules, next_segment_id, content_store) = if Path::new(&metadata_path).exists() {
            let data = fs::read_to_string(&metadata_path)?;
            let state: RegistryState = serde_json::from_str(&data)?;
            (state.capsules, state.next_segment_id, state.content_store)
        } else {
            (HashMap::new(), 0, HashMap::new())
        };

        Ok(Self {
            capsules: Arc::new(RwLock::new(capsules)),
            next_segment_id: Arc::new(RwLock::new(next_segment_id)),
            metadata_path,
            content_store: Arc::new(RwLock::new(content_store)),
        })
    }

    pub fn save(&self) -> Result<()> {
        let state = RegistryState {
            capsules: self.capsules.read().unwrap().clone(),
            next_segment_id: *self.next_segment_id.read().unwrap(),
            content_store: self.content_store.read().unwrap().clone(),
        };
        
        let json = serde_json::to_string_pretty(&state)?;
        fs::write(&self.metadata_path, json)?;
        Ok(())
    }

    pub fn create_capsule_with_segments(&self, id: CapsuleId, size: u64, segments: Vec<SegmentId>) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();
        
        if capsules.contains_key(&id) {
            anyhow::bail!("Capsule collision (extremely unlikely)");
        }

        let capsule = Capsule {
            id,
            size,
            segments,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            policy: Policy::default(),
            deduped_bytes: 0,  // Will be updated during write
        };

        capsules.insert(id, capsule);
        drop(capsules);
        self.save()?;
        Ok(())
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
        self.save()?;
        Ok(())
    }

    // NEW: Phase 2.2 - Deduplication methods
    
    /// Check if content hash already exists in store
    pub fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        self.content_store.read().unwrap().get(hash).copied()
    }

    /// Register new content hash → segment mapping
    pub fn register_content(&self, hash: ContentHash, seg_id: SegmentId) -> Result<()> {
        self.content_store.write().unwrap().insert(hash, seg_id);
        self.save()?;
        Ok(())
    }

    /// Increment dedup bytes counter for a capsule
    pub fn add_deduped_bytes(&self, capsule_id: CapsuleId, bytes: u64) -> Result<()> {
        let mut capsules = self.capsules.write().unwrap();
        if let Some(capsule) = capsules.get_mut(&capsule_id) {
            capsule.deduped_bytes += bytes;
        }
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

    /// Get dedup statistics (for debugging/monitoring)
    pub fn get_dedup_stats(&self) -> (usize, usize) {
        let content_store = self.content_store.read().unwrap();
        let capsules = self.capsules.read().unwrap();
        
        let total_segments: usize = capsules.values()
            .map(|c| c.segments.len())
            .sum();
        
        let unique_segments = content_store.len();
        
        (total_segments, unique_segments)
    }
}

impl Default for CapsuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
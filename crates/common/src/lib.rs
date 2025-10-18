use uuid::Uuid;
use serde::{Serialize, Deserialize};

pub mod policy;
pub use policy::{Policy, CompressionPolicy};

pub const SEGMENT_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapsuleId(pub Uuid);

impl CapsuleId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
    
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for CapsuleId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capsule {
    pub id: CapsuleId,
    pub size: u64,
    pub segments: Vec<SegmentId>,
    pub created_at: u64,
    
    // Phase 2: Add policy and stats
    #[serde(default)]
    pub policy: Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,
    pub len: u32,
    
    // Phase 2: Track compression and access patterns
    #[serde(default)]
    pub compressed: bool,
    #[serde(default)]
    pub compression_algo: String, // "lz4", "zstd", or "none"
    #[serde(default)]
    pub deduplicated: bool,
    #[serde(default)]
    pub access_count: u32,
}
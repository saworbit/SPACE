use uuid::Uuid;

pub const SEGMENT_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapsuleId(pub Uuid);

impl CapsuleId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone)]
pub struct Capsule {
    pub id: CapsuleId,
    pub size: u64,
    pub segments: Vec<SegmentId>,
    pub created_at: u64,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,  // Byte offset in NVRAM log
    pub len: u32,     // Actual data length
}
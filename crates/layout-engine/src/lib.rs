pub mod compiler;
pub mod ml;
pub mod offload;
pub mod zns;

use anyhow::Result;
use common::{CapsuleId, ContentHash, Policy};

/// Zone plan describing layout decisions for one or more capsules.
pub struct ZonePlan {
    pub zones: Vec<Zone>,
    pub merkle_root: Option<ContentHash>,
}

/// Physical zone with deterministic IV seed.
pub struct Zone {
    pub id: u64,
    pub iv_seed: u64,
    pub segments: Vec<SegmentRef>,
}

/// Reference to a segment belonging to a capsule.
pub struct SegmentRef {
    pub capsule_id: CapsuleId,
    pub offset: u64,
    pub length: u64,
    pub compressed_hash: ContentHash,
}

/// Trait implemented by each hardware/software offload.
pub trait LayoutOffload {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan>;
}

/// Engine that routes layout requests to compiled offloads.
pub struct LayoutEngine {
    offload: Box<dyn LayoutOffload + Send + Sync>,
}

impl LayoutEngine {
    pub fn new(policy: &Policy) -> Self {
        let offload = compiler::compile(policy);
        Self { offload }
    }

    pub fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan> {
        self.offload.synthesize(capsules, data_slices, policy)
    }
}

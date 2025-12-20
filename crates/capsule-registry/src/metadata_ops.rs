use common::{Capsule, CapsuleId, ContentHash, SegmentId};
use serde::{Deserialize, Serialize};

/// Operations replicated through the Raft log.
#[allow(dead_code)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataOp {
    PutCapsule(Capsule),
    DeleteCapsule(CapsuleId),
    RegisterContent {
        hash: ContentHash,
        segment: SegmentId,
    },
    DeregisterContent {
        hash: ContentHash,
        segment: SegmentId,
    },
}

/// State machine responses surfaced to callers.
#[allow(dead_code)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpResult {
    Ok,
    CapsuleFound(Capsule),
    NotFound,
    Error(String),
}

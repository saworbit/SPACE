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
    PutBucket {
        bucket: String,
        created_at: u64,
    },
    PutObject {
        bucket: String,
        key: String,
        capsule: CapsuleId,
    },
    DeleteObject {
        bucket: String,
        key: String,
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
    /// The capsule an S3 key pointed at before the operation.
    Replaced(Option<CapsuleId>),
    Error(String),
}

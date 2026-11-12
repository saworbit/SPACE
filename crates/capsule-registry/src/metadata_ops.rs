use common::{Capsule, CapsuleId};
use serde::{Deserialize, Serialize};

/// S3 namespace operations. Capsule records and the content index go
/// through the capsule journal instead.
#[allow(dead_code)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataOp {
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

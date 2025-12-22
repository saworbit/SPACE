use serde::{Deserialize, Serialize};

/// Pointer left behind when a segment payload is tiered out of hot storage.
///
/// Serialized as JSON and stored in place of the original segment bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageStub {
    /// Version marker used to distinguish stubs from real segment payloads.
    pub magic: String, // "SPACE_STUB_V1"
    /// Original byte length of the segment payload that was offloaded.
    pub original_size: u64,
    /// Remote location (e.g. `s3://bucket/segments/<id>.bin`).
    pub remote_url: String,
    /// Integrity checksum string (e.g. `sha256:<hex>`).
    pub checksum: String,
}

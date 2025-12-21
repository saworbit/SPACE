use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use common::SegmentId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StubBackend {
    SimulatedS3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentStub {
    pub segment_id: u64,
    pub backend: StubBackend,
    pub remote_key: String,
    pub migrated_at: u64,
}

impl SegmentStub {
    pub fn new_simulated_s3(segment: SegmentId, remote_key: String) -> Self {
        Self {
            segment_id: segment.0,
            backend: StubBackend::SimulatedS3,
            remote_key,
            migrated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).context("serialize segment stub")
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("deserialize segment stub")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read stub {}", path.display()))?;
        Self::from_json_bytes(&bytes)
    }

    pub fn validate_for_segment(&self, segment: SegmentId) -> Result<()> {
        if self.segment_id != segment.0 {
            return Err(anyhow!(
                "stub segment id mismatch: expected {}, got {}",
                segment.0,
                self.segment_id
            ));
        }
        Ok(())
    }

    pub fn cold_object_path(&self, cold_root: &Path) -> PathBuf {
        cold_root.join(&self.remote_key)
    }
}

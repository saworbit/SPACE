//! Snapshot Engine: Point-in-time persistence of Volumes to Capsules.
//!
//! The Snapshot Engine bridges the gap between Foundry's ephemeral, high-speed
//! volumes and the Capsule Registry's immortal, deduplicated storage.
//!
//! ## Architecture
//!
//! - **Chunking**: Volumes are split into 64KB blocks for efficient deduplication
//! - **Deduplication**: Identical blocks (e.g., zeros, common OS files) stored once globally
//! - **Manifest**: A JSON capsule containing the map of Block Offsets -> Capsule IDs
//!
//! ## Usage
//!
//! ```ignore
//! use foundry::snapshot::SnapshotEngine;
//! use capsule_registry::pipeline::WritePipeline;
//! use common::Policy;
//! use std::sync::Arc;
//!
//! # async fn example() -> anyhow::Result<()> {
//! # let pipeline = todo!();
//! # let volume_id = todo!();
//! # let volume = todo!();
//! let engine = SnapshotEngine::new(pipeline);
//!
//! // Take a snapshot
//! let manifest_id = engine.take_snapshot(
//!     volume_id,
//!     volume.clone(),
//!     Policy::default()
//! ).await?;
//!
//! // Restore from snapshot
//! engine.restore_snapshot(volume_id, manifest_id, volume).await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};

use crate::backend::{VolumeBackend, VolumeId};
use crate::error::{FoundryError, Result};
use capsule_registry::pipeline::WritePipeline;
use common::{CapsuleId, Policy};

/// Standard chunk size for snapshotting.
/// 64KB is a sweet spot: small enough for good dedup, large enough to amortize metadata overhead.
const SNAPSHOT_CHUNK_SIZE: usize = 64 * 1024;

/// The blueprint of a volume at a specific point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub volume_id: VolumeId,
    pub size_bytes: u64,
    pub created_at: u64, // Unix timestamp
    pub blocks: Vec<BlockMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMetadata {
    pub offset: u64,
    pub len: u32,
    pub capsule_id: CapsuleId,
}

/// Orchestrates the movement of data between Foundry (Hot) and Registry (Cold).
pub struct SnapshotEngine {
    pipeline: Arc<WritePipeline>,
}

impl SnapshotEngine {
    pub fn new(pipeline: Arc<WritePipeline>) -> Self {
        Self { pipeline }
    }

    /// Captures the current state of a volume and persists it as a Capsule.
    ///
    /// Returns the CapsuleId of the Manifest.
    #[instrument(skip(self, volume, policy), fields(volume_id = %volume_id))]
    pub async fn take_snapshot(
        &self,
        volume_id: VolumeId,
        volume: Arc<dyn VolumeBackend>,
        policy: Policy,
    ) -> Result<CapsuleId> {
        let size = volume.size().await?;
        info!(
            "Starting snapshot for volume {}, size: {} bytes",
            volume_id, size
        );

        let mut blocks = Vec::new();
        let mut offset = 0;

        // FUTURE OPTIMIZATION:
        // Use `lseek(SEEK_DATA)` (if supported by backend) to skip holes in sparse files.
        // Currently, we read the zeros and let the Pipeline dedup them (Global Zero Block).
        while offset < size {
            let len = std::cmp::min(SNAPSHOT_CHUNK_SIZE as u64, size - offset) as usize;

            // 1. Read from Hot Storage
            let data = volume.read_at(offset, len).await?;

            // 2. Write to Cold Storage (Pipeline handles encryption/dedup)
            // We clone the policy to ensure every block adheres to retention/security rules.
            let capsule_id = self
                .pipeline
                .write_capsule_with_policy(&data, &policy)
                .await
                .map_err(|e| {
                    FoundryError::io_error(offset, std::io::Error::other(e.to_string()))
                })?;

            blocks.push(BlockMetadata {
                offset,
                len: len as u32,
                capsule_id,
            });

            offset += len as u64;
        }

        // 3. Seal the Manifest
        let manifest = SnapshotManifest {
            volume_id,
            size_bytes: size,
            created_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            blocks,
        };

        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| {
            FoundryError::config_error(format!("Failed to serialize manifest: {}", e))
        })?;

        // 4. Write the Manifest itself as a Capsule
        let manifest_id = self
            .pipeline
            .write_capsule_with_policy(&manifest_bytes, &policy)
            .await
            .map_err(|e| FoundryError::config_error(format!("Failed to write manifest: {}", e)))?;

        info!(manifest_id = ?manifest_id, "Snapshot committed successfully");
        Ok(manifest_id)
    }

    /// Restores a volume state from a Snapshot Manifest.
    ///
    /// WARNING: This is a destructive operation for the target volume region.
    #[instrument(skip(self, volume), fields(volume_id = %volume_id, manifest_id = ?manifest_id))]
    pub async fn restore_snapshot(
        &self,
        volume_id: VolumeId,
        manifest_id: CapsuleId,
        volume: Arc<dyn VolumeBackend>,
    ) -> Result<()> {
        info!("Restoring snapshot...");

        // 1. Fetch the Manifest
        let manifest_bytes = self
            .pipeline
            .read_capsule(manifest_id)
            .await
            .map_err(|_e| FoundryError::VolumeNotFound(volume_id))?; // Mapping registry error to foundry error for now

        let manifest: SnapshotManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| FoundryError::config_error(format!("Invalid manifest format: {}", e)))?;

        // 2. Adjust Volume Size
        let current_size = volume.size().await?;
        if current_size < manifest.size_bytes {
            info!(
                "Resizing volume from {} to {}",
                current_size, manifest.size_bytes
            );
            volume.resize(manifest.size_bytes).await?;
        }

        // 3. Rehydrate Blocks
        for block in manifest.blocks {
            // Retrieve data (Pipeline handles decryption/decompression)
            let data = self
                .pipeline
                .read_capsule(block.capsule_id)
                .await
                .map_err(|e| {
                    FoundryError::io_error(block.offset, std::io::Error::other(e.to_string()))
                })?;

            if data.len() != block.len as usize {
                warn!(
                    "Block length mismatch at offset {}. Manifest: {}, Actual: {}",
                    block.offset,
                    block.len,
                    data.len()
                );
                // We proceed, but this indicates corruption or a partial read in pipeline
            }

            volume.write_at(block.offset, Bytes::from(data)).await?;
        }

        // 4. Flush to Disk
        volume.sync().await?;

        info!("Restore complete.");
        Ok(())
    }
}

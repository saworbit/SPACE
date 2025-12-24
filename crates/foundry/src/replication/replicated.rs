//! Replicated backend wrapper for chain replication.
//!
//! This module provides a `ReplicatedBackend` that wraps any `VolumeBackend`
//! and synchronously replicates writes to a replica node before acknowledging
//! to the client.

use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::backend::VolumeBackend;
use crate::error::Result;

use super::actor::ReplicationClient;

/// A volume backend that replicates writes to a secondary node.
///
/// This wrapper implements synchronous chain replication:
/// 1. Write to local backend (can be parallel with replication)
/// 2. Replicate to secondary node
/// 3. Wait for both to complete
/// 4. Return success only if both succeed
///
/// Reads are served locally without involving the replica.
pub struct ReplicatedBackend {
    local: Arc<dyn VolumeBackend>,
    replicator: Arc<ReplicationClient>,
}

impl ReplicatedBackend {
    /// Create a new replicated backend.
    ///
    /// # Arguments
    ///
    /// * `local` - The local volume backend to write to
    /// * `replicator` - The replication client connected to the replica
    pub fn new(local: Arc<dyn VolumeBackend>, replicator: ReplicationClient) -> Self {
        Self {
            local,
            replicator: Arc::new(replicator),
        }
    }
}

impl VolumeBackend for ReplicatedBackend {
    fn init(&self, size_bytes: u64) -> BoxFuture<'_, Result<()>> {
        // Only initialize the local backend
        // The replica backend should be initialized separately
        self.local.init(size_bytes)
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, Result<Bytes>> {
        // Reads are served locally
        self.local.read_at(offset, len)
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, Result<()>> {
        // Clone necessary values for the async block
        let local = self.local.clone();
        let replicator = self.replicator.clone();
        let data_clone = data.clone();

        Box::pin(async move {
            // Execute local write and replication in parallel
            // Both must succeed for the write to be acknowledged
            let local_fut = local.write_at(offset, data);
            let remote_fut = replicator.replicate(offset, &data_clone);

            let (local_res, remote_res) = tokio::join!(local_fut, remote_fut);

            // Check both results
            local_res?;
            remote_res?;

            Ok(())
        })
    }

    fn sync(&self) -> BoxFuture<'_, Result<()>> {
        // Sync only the local backend
        // Replication is already synchronous
        self.local.sync()
    }

    fn size(&self) -> BoxFuture<'_, Result<u64>> {
        // Return the local volume size
        self.local.size()
    }

    fn resize(&self, new_size: u64) -> BoxFuture<'_, Result<()>> {
        // Forward to local backend
        // Note: In a production system, you'd want to coordinate resize
        // with the replica, but this is beyond scope for Phase 8.4
        self.local.resize(new_size)
    }
}

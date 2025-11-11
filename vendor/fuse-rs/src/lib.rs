//! Lightweight FUSE helper used for Phase 4 local mounts.

use anyhow::Result;
use tracing::{debug, info};

/// Simplified filesystem implementation that wraps capsule data.
#[derive(Debug, Clone)]
pub struct FilesystemImpl {
    data: Vec<u8>,
}

impl FilesystemImpl {
    /// Create a FUSE view for the capsule data.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Mount the filesystem at the given mountpoint.
    pub fn mount(self, mountpoint: &str) -> Result<MountHandle> {
        info!(mountpoint = %mountpoint, "fuse: mounting capsule filesystem");
        // In a real implementation this would call fuse::mount
        Ok(MountHandle {
            mountpoint: mountpoint.to_string(),
            mounted_data: self.data,
        })
    }
}

/// Handle representing a mounted FUSE view.
#[derive(Debug)]
pub struct MountHandle {
    mountpoint: String,
    mounted_data: Vec<u8>,
}

impl MountHandle {
    /// Unmount the view.
    pub fn unmount(self) -> Result<()> {
        debug!(mountpoint = %self.mountpoint, "fuse: unmounting view");
        Ok(())
    }

    /// Inspect the mountpoint path.
    pub fn mountpoint(&self) -> &str {
        &self.mountpoint
    }
}

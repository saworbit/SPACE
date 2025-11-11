//! Minimal NFS helper used by the Phase 4 view exports.

use anyhow::Result;
use tracing::info;

/// Options for exporting a capsule via NFS.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub path: String,
}

impl ExportOptions {
    /// Create a new export pointing at the capsule data path.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

/// Simple NFS server mock for Phase 4 demonstrations.
#[derive(Debug)]
pub struct NfsServer {
    exports: Vec<(String, ExportOptions)>,
}

impl NfsServer {
    /// Start a builder for an NFS server.
    pub fn new() -> Self {
        Self {
            exports: Vec::new(),
        }
    }

    /// Export a capsule under the specified name.
    pub fn export(&mut self, name: String, options: ExportOptions) -> &mut Self {
        self.exports.push((name, options));
        self
    }

    /// Start serving the configured exports.
    pub async fn start(self) -> Result<Self> {
        info!(exports = self.exports.len(), "nfs: starting exports");
        Ok(self)
    }
}

impl Default for NfsServer {
    fn default() -> Self {
        Self::new()
    }
}

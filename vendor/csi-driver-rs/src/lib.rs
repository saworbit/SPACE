//! CSI driver helper used by Phase 4 integration tests.

use anyhow::Result;
use tracing::info;

/// Request to provision a CSI volume mapped to a capsule.
#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    pub capsule_id: String,
}

impl ProvisionRequest {
    /// Create a request from a capsule identifier string.
    pub fn from_capsule(capsule_id: &str) -> Self {
        Self {
            capsule_id: capsule_id.to_string(),
        }
    }
}

/// Simplified CSI server stub.
#[derive(Debug)]
pub struct CsiServer {
    capsule_id: String,
}

impl CsiServer {
    /// Provision a CSI volume for the requested capsule.
    pub fn provision(capsule_id: &str) -> Result<Self> {
        info!(capsule = %capsule_id, "csi: provisioning capsule volume");
        Ok(Self {
            capsule_id: capsule_id.to_string(),
        })
    }

    /// Inspect the capsule associated with this server.
    pub fn capsule_id(&self) -> &str {
        &self.capsule_id
    }
}

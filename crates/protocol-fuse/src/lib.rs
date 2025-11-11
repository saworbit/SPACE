//! Phase 4 FUSE view helpers.
#![cfg(feature = "phase4")]

use anyhow::Result;
use capsule_registry::CapsuleRegistry;
use common::{CapsuleId, Policy};
use tracing::info;

/// Simple representation of a mounted FUSE view.
pub struct FuseMount {
    capsule_id: CapsuleId,
    mountpoint: String,
}

impl FuseMount {
    /// The mountpoint exposed to userspace.
    pub fn mountpoint(&self) -> &str {
        &self.mountpoint
    }
}

/// Mounts a capsule as a local FUSE filesystem.
pub fn mount_fuse_view(
    id: CapsuleId,
    policy: &Policy,
    mountpoint: &str,
    registry: &CapsuleRegistry,
) -> Result<FuseMount> {
    registry.lookup(id)?;
    info!(
        capsule = %id.as_uuid(),
        mountpoint,
        "registering FUSE mountpoint"
    );

    Ok(FuseMount {
        capsule_id: id,
        mountpoint: mountpoint.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuse_mount_returns_path() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::default();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mount = mount_fuse_view(capsule_id, &policy, "/mnt/fuse", &registry).unwrap();
        assert_eq!(mount.mountpoint(), "/mnt/fuse");
    }
}

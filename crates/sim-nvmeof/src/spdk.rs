//! SPDK-backed NVMe-oF target wiring (feature-gated).
//!
//! The bindings are optional and guarded by the `spdk` Cargo feature. This
//! module is only compiled when the feature is enabled.

use crate::config::NvmeofSimConfig;
use anyhow::{anyhow, Result};
use tracing::info;

/// Attempt to start the SPDK-backed target. Returns an error if initialization
/// cannot proceed so the orchestrator can fall back to the native TCP path.
pub fn start_spdk_target(config: &NvmeofSimConfig) -> Result<()> {
    info!("Initializing SPDK environment (bindings detected)");

    // Exercise the bindings to catch build-time issues early.
    let mut builder = spdk_rs::NvmeTargetBuilder::new();
    builder.add_namespace(spdk_rs::Namespace::new(
        config.backing_path.as_bytes().to_vec(),
    ));
    let target = builder.build();

    info!(
        namespaces = target.namespaces().len(),
        nqn = %config.subsystem_nqn,
        "SPDK NVMe target constructed"
    );

    Err(anyhow!(
        "SPDK runtime integration not implemented in sim; falling back to native TCP"
    ))
}

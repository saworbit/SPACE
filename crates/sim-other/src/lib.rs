//! Placeholder for future simulation modules.
//!
//! This crate serves as a stub for extending SPACE's simulation capabilities
//! beyond NVRAM and NVMe-oF. Potential use cases include:
//!
//! - **GPU Offload Simulation**: Mock GPU compression/dedup for CapsuleFlow
//! - **ZNS (Zoned Namespace) SSD**: Simulate append-only zones
//! - **Network Conditions**: Latency/packet loss injection for mesh testing
//! - **Hardware Accelerators**: DPU, SmartNIC, or other offload engines
//!
//! # Design Philosophy
//!
//! This crate is intentionally minimal, providing a clear extension point
//! for future development without adding dependencies to the core workspace.
//!
//! # Example
//!
//! ```
//! use sim_other::start_other_sim;
//!
//! // When implemented, this might start a GPU offload simulator
//! start_other_sim().unwrap();
//! ```

use anyhow::Result;
use tracing::info;

/// Start a placeholder simulation.
///
/// Currently a no-op, but serves as the entry point for future sim modules.
/// See module-level docs for potential extensions.
///
/// # Example
///
/// ```
/// use sim_other::start_other_sim;
///
/// start_other_sim().unwrap();
/// ```
pub fn start_other_sim() -> Result<()> {
    info!("Placeholder for other sim modules (GPU offload, ZNS, etc.)");
    info!("To add a new simulation:");
    info!("  1. Add feature to sim-other/Cargo.toml (e.g., 'gpu-offload')");
    info!("  2. Implement module in sim-other/src/<module>.rs");
    info!("  3. Update sim container entrypoint to recognize SIM_MODULES=<module>");
    Ok(())
}

/// Example stub for future GPU offload simulation.
///
/// This function demonstrates how a future GPU sim module might be structured.
#[cfg(feature = "gpu-offload")]
pub fn start_gpu_offload_sim() -> Result<()> {
    info!("Starting GPU offload simulation (placeholder)");
    // Future implementation:
    // 1. Mock CUDA/OpenCL environment
    // 2. Simulate compression/dedup on "GPU"
    // 3. Integrate with CapsuleFlow pipeline
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_other_sim_placeholder() {
        // Should not error, just log
        start_other_sim().unwrap();
    }

    #[cfg(feature = "gpu-offload")]
    #[test]
    fn test_gpu_offload_stub() {
        start_gpu_offload_sim().unwrap();
    }
}

//! Lightweight NVRAM simulation wrapper for testing SPACE pipelines.
//!
//! This crate extends the core `nvram-sim` with simulation-specific behaviors
//! and configuration, enabling end-to-end testing of data management features
//! (compression, deduplication, encryption) without physical hardware.
//!
//! # Design Philosophy
//!
//! - **Modularity**: Separate from core `nvram-sim` to avoid test artifacts in production
//! - **Lightweight**: File-backed or RAM-backed log emulation with minimal overhead
//! - **Fault Injection**: Optional hooks for resilience testing (future)
//!
//! # Example
//!
//! ```no_run
//! use sim_nvram::start_nvram_sim;
//!
//! let log = start_nvram_sim("test_nvram.sim").unwrap();
//! // Use log in pipeline tests
//! ```

use anyhow::Result;
use nvram_sim::{NvramLog, NvramTransaction};
use common::SegmentId;
use tracing::{info, debug};

/// Configuration for NVRAM simulation.
///
/// Allows customization of backing storage and optional fault injection.
#[derive(Debug, Clone)]
pub struct NvramSimConfig {
    /// Path to backing file (or ":memory:" for RAM-backed)
    pub backing_path: String,
    /// Enable fault injection (e.g., random I/O errors for resilience tests)
    pub enable_fault_injection: bool,
    /// Simulated latency in microseconds (for performance testing)
    pub simulated_latency_us: u64,
}

impl Default for NvramSimConfig {
    fn default() -> Self {
        Self {
            backing_path: "sim_nvram.log".to_string(),
            enable_fault_injection: false,
            simulated_latency_us: 0,
        }
    }
}

/// Start a simulated NVRAM log for testing pipelines.
///
/// This function wraps `nvram_sim::NvramLog::open` with simulation-specific
/// configuration. It's the primary entry point for integration tests.
///
/// # Arguments
///
/// * `backing_path` - Path to backing file, or ":memory:" for RAM-backed
///
/// # Returns
///
/// A `NvramLog` instance ready for use in pipeline tests.
///
/// # Example
///
/// ```no_run
/// use sim_nvram::start_nvram_sim;
///
/// let log = start_nvram_sim("test.log").unwrap();
/// ```
pub fn start_nvram_sim(backing_path: &str) -> Result<NvramLog> {
    info!(backing_path, "Starting NVRAM simulation");

    // Use existing NvramLog::open, but with sim config
    let log = NvramLog::open(backing_path)?;

    debug!("NVRAM simulation initialized successfully");

    // Future: Add fault injection hooks here
    // if config.enable_fault_injection { ... }

    Ok(log)
}

/// Start a configured NVRAM simulation.
///
/// Provides more control than `start_nvram_sim`, accepting a full config.
///
/// # Example
///
/// ```no_run
/// use sim_nvram::{start_nvram_sim_with_config, NvramSimConfig};
///
/// let config = NvramSimConfig {
///     backing_path: ":memory:".to_string(),
///     enable_fault_injection: true,
///     simulated_latency_us: 100,
/// };
/// let log = start_nvram_sim_with_config(config).unwrap();
/// ```
pub fn start_nvram_sim_with_config(config: NvramSimConfig) -> Result<NvramLog> {
    info!(?config, "Starting NVRAM simulation with custom config");

    let log = NvramLog::open(&config.backing_path)?;

    if config.simulated_latency_us > 0 {
        debug!(latency_us = config.simulated_latency_us, "Simulated latency enabled");
        // Future: Wrap log methods with std::thread::sleep
    }

    if config.enable_fault_injection {
        debug!("Fault injection enabled (placeholder for future implementation)");
        // Future: Return a FaultInjectingNvramLog wrapper
    }

    Ok(log)
}

/// Helper to create a transaction on a simulated log.
///
/// Convenience wrapper for common test patterns.
pub fn create_sim_transaction(log: &NvramLog) -> Result<NvramTransaction> {
    log.begin_transaction()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nvram_sim_basic() {
        let log = start_nvram_sim("test_nvram_basic.sim").unwrap();
        let seg_id = SegmentId(1);
        let data = b"test data for sim-nvram";

        let segment = log.append(seg_id, data).unwrap();
        assert_eq!(segment.len, data.len() as u32);

        let read_data = log.read(seg_id).unwrap();
        assert_eq!(read_data, data);

        // Cleanup
        std::fs::remove_file("test_nvram_basic.sim").ok();
        std::fs::remove_file("test_nvram_basic.sim.segments").ok();
    }

    #[test]
    fn test_nvram_sim_with_config() {
        let config = NvramSimConfig {
            backing_path: "test_nvram_config.sim".to_string(),
            enable_fault_injection: false,
            simulated_latency_us: 0,
        };

        let log = start_nvram_sim_with_config(config).unwrap();
        let seg_id = SegmentId(2);

        log.append(seg_id, b"config test").unwrap();
        let data = log.read(seg_id).unwrap();
        assert_eq!(data, b"config test");

        // Cleanup
        std::fs::remove_file("test_nvram_config.sim").ok();
        std::fs::remove_file("test_nvram_config.sim.segments").ok();
    }

    #[test]
    fn test_sim_transaction() {
        let log = start_nvram_sim("test_nvram_tx.sim").unwrap();
        let mut tx = create_sim_transaction(&log).unwrap();

        let seg_id = SegmentId(3);
        tx.append_segment(seg_id, b"transactional data").unwrap();
        tx.commit().unwrap();

        let data = log.read(seg_id).unwrap();
        assert_eq!(data, b"transactional data");

        // Cleanup
        std::fs::remove_file("test_nvram_tx.sim").ok();
        std::fs::remove_file("test_nvram_tx.sim.segments").ok();
    }
}

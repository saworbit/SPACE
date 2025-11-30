//! NVMe-oF Simulation Orchestrator.
//!
//! Selects between the high-performance SPDK simulation (if compiled & available)
//! and the compatible Native Rust TCP fallback.

use anyhow::Result;
use tracing::info;
#[cfg(feature = "spdk")]
use tracing::{error, warn};

pub mod config;
pub mod native;
#[cfg(feature = "spdk")]
pub mod spdk;

pub use config::NvmeofSimConfig;

/// Start the NVMe-oF simulation with a node identifier and backing file path.
pub fn start_nvmeof_sim(node_id: &str, backing_path: &str) -> Result<()> {
    let config = NvmeofSimConfig {
        node_id: node_id.to_string(),
        backing_path: backing_path.to_string(),
        ..Default::default()
    };
    start_nvmeof_sim_with_config(config)
}

/// Entry point for the NVMe-oF simulation.
/// Attempts SPDK if compiled and ready, otherwise falls back to the native TCP target.
pub fn start_nvmeof_sim_with_config(config: NvmeofSimConfig) -> Result<()> {
    info!(node_id = %config.node_id, "Initializing NVMe-oF Simulation");

    // 1. Attempt SPDK path if compiled in
    #[cfg(feature = "spdk")]
    {
        if is_spdk_environment_ready() {
            info!("SPDK environment detected. Attempting to start SPDK target...");
            match spdk::start_spdk_target(&config) {
                Ok(_) => return Ok(()), // SPDK takes over thread, so if it returns Ok, it finished cleanly
                Err(e) => {
                    error!(error = %e, "SPDK initialization failed. Falling back to Native TCP.");
                }
            }
        } else {
            warn!("SPDK feature enabled but environment not ready (missing hugepages/limits/privileges). Using Native TCP.");
        }
    }

    #[cfg(not(feature = "spdk"))]
    {
        info!("SPDK feature not compiled. Using Native TCP.");
    }

    // 2. Fallback to Native Rust implementation
    native::start_native_tcp_target(config)
}

/// Verifies runtime prerequisites for SPDK on Linux hosts.
#[cfg(all(feature = "spdk", target_os = "linux"))]
fn is_spdk_environment_ready() -> bool {
    // Check 1: Hugepages
    if !check_hugepages() {
        warn!("Check failed: Hugepages not configured or insufficient free space.");
        return false;
    }

    // Check 2: Memory Lock Limits (ulimit -l)
    if !check_memlock_limit() {
        warn!("Check failed: RLIMIT_MEMLOCK too low for SPDK.");
        return false;
    }

    // Check 3: Root privileges (simplified check)
    if unsafe { libc::geteuid() } != 0 {
        warn!("Check failed: Process not running as root (required for SPDK).");
        return false;
    }

    true
}

/// SPDK is not available on non-Linux targets; always fall back.
#[cfg(all(feature = "spdk", not(target_os = "linux")))]
fn is_spdk_environment_ready() -> bool {
    warn!("SPDK feature enabled but only supported on Linux targets; using Native TCP fallback.");
    false
}

/// Validate hugepage availability (>512MB free).
#[cfg(all(feature = "spdk", target_os = "linux"))]
fn check_hugepages() -> bool {
    use std::fs;

    let meminfo = match fs::read_to_string("/proc/meminfo") {
        Ok(data) => data,
        Err(_) => return false,
    };

    let mut hugepage_size_kb: Option<u64> = None;
    let mut hugepages_free: Option<u64> = None;

    for line in meminfo.lines() {
        if line.starts_with("HugePages_Free:") {
            if let Some(value) = line.split_whitespace().nth(1) {
                hugepages_free = value.parse::<u64>().ok();
            }
        } else if line.starts_with("Hugepagesize:") {
            if let Some(value) = line.split_whitespace().nth(1) {
                hugepage_size_kb = value.parse::<u64>().ok();
            }
        }
    }

    match (hugepage_size_kb, hugepages_free) {
        (Some(size_kb), Some(free_pages)) => {
            let free_bytes = size_kb * free_pages * 1024;
            free_bytes >= 512 * 1024 * 1024
        }
        _ => false,
    }
}

/// Validate memlock limit (>=512MB or unlimited).
#[cfg(all(feature = "spdk", target_os = "linux"))]
fn check_memlock_limit() -> bool {
    use std::mem::MaybeUninit;

    let mut rlim = MaybeUninit::<libc::rlimit>::uninit();
    let res = unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, rlim.as_mut_ptr()) };
    if res != 0 {
        return false;
    }

    let rlim = unsafe { rlim.assume_init() };
    let required: libc::rlim_t = 512 * 1024 * 1024;

    rlim.rlim_cur == libc::RLIM_INFINITY || rlim.rlim_cur >= required
}

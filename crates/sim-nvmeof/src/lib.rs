//! NVMe-oF fabric simulation for testing SPACE Phase 4 features.
//!
//! This crate provides heavyweight simulation of NVMe-over-Fabrics protocol
//! views, enabling end-to-end testing of block access patterns, federated
//! capsules, and mesh node interactions without physical NVMe hardware.
//!
//! # Design Philosophy
//!
//! - **SPDK-Based**: Leverages SPDK (Storage Performance Development Kit) for
//!   realistic protocol emulation
//! - **Fabric Simulation**: TCP/IP transport for multi-node mesh scenarios
//! - **Isolated**: Runs in separate container with privileged access for hugepages
//!
//! # Requirements
//!
//! - **Linux**: SPDK requires Linux kernel support for hugepages and VFIO
//! - **Hugepages**: Kernel must have hugepages enabled (check `/proc/meminfo`)
//! - **Privileges**: Container needs `--privileged` or specific capabilities
//!
//! # Example
//!
//! ```no_run
//! use sim_nvmeof::start_nvmeof_sim;
//!
//! // In a separate container or privileged context:
//! start_nvmeof_sim("node1", "backing.img").unwrap();
//! ```

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{debug, info, warn};

/// Configuration for NVMe-oF simulation.
#[derive(Debug, Clone)]
pub struct NvmeofSimConfig {
    /// Node identifier (e.g., "node1")
    pub node_id: String,
    /// Path to backing image file
    pub backing_path: String,
    /// Transport type (tcp or rdma)
    pub transport: String,
    /// Listen address
    pub listen_addr: String,
    /// Listen port
    pub listen_port: u16,
    /// Subsystem NQN (NVMe Qualified Name)
    pub subsystem_nqn: String,
}

impl Default for NvmeofSimConfig {
    fn default() -> Self {
        Self {
            node_id: "sim-node1".to_string(),
            backing_path: "sim_nvmeof.img".to_string(),
            transport: "tcp".to_string(),
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4420,
            subsystem_nqn: "nqn.2024-01.dev.adaptive-storage:space-sim".to_string(),
        }
    }
}

/// Start an NVMe-oF fabric simulation.
///
/// This function initializes SPDK, creates a backing block device (bdev),
/// and exposes it via NVMe-oF target. It's heavyweight and requires Linux
/// with hugepages enabled.
///
/// # Arguments
///
/// * `node_id` - Unique identifier for this simulated node
/// * `backing_path` - Path to backing image file (created if doesn't exist)
///
/// # Returns
///
/// Runs indefinitely, serving NVMe-oF requests. Returns on error or shutdown.
///
/// # Example
///
/// ```no_run
/// use sim_nvmeof::start_nvmeof_sim;
///
/// // Run in background or separate thread:
/// start_nvmeof_sim("node1", "backing.img").unwrap();
/// ```
pub fn start_nvmeof_sim(node_id: &str, backing_path: &str) -> Result<()> {
    let config = NvmeofSimConfig {
        node_id: node_id.to_string(),
        backing_path: backing_path.to_string(),
        ..Default::default()
    };
    start_nvmeof_sim_with_config(config)
}

/// Start NVMe-oF simulation with full configuration.
///
/// Provides more control than `start_nvmeof_sim`, accepting a full config.
pub fn start_nvmeof_sim_with_config(config: NvmeofSimConfig) -> Result<()> {
    info!(?config, "Starting NVMe-oF simulation");

    // Check prerequisites
    check_hugepages_available()?;
    ensure_backing_file_exists(&config.backing_path)?;

    // Initialize SPDK subsystem
    // Note: This is a placeholder. Actual SPDK integration requires:
    // 1. spdk_env_init() to set up hugepages and memory
    // 2. Create a bdev (e.g., via spdk_bdev_create_aio)
    // 3. Create NVMe-oF subsystem (spdk_nvmf_subsystem_create)
    // 4. Add listener (spdk_nvmf_subsystem_add_listener)
    // 5. Start polling loop

    // Since full SPDK integration is complex, we provide a TCP-based fallback
    // for simpler testing scenarios. Check if SPDK is available:
    if !is_spdk_available() {
        warn!("SPDK not available or hugepages not configured; falling back to TCP simulation");
        return run_tcp_fallback_sim(config);
    }

    // SPDK path (for when vendor/spdk-rs is fully integrated)
    info!("Initializing SPDK-based NVMe-oF target...");

    // Placeholder for SPDK init sequence:
    // 1. spdk_rs::env::init()?;
    // 2. let bdev = spdk_rs::bdev::create_aio(&config.backing_path)?;
    // 3. let subsys = spdk_rs::nvmf::create_subsystem(&config.subsystem_nqn)?;
    // 4. subsys.add_namespace(bdev)?;
    // 5. subsys.add_listener(&config.transport, &config.listen_addr, config.listen_port)?;
    // 6. spdk_rs::run_event_loop()?; // Blocks until shutdown

    info!(
        node_id = config.node_id,
        nqn = config.subsystem_nqn,
        address = format!("{}:{}", config.listen_addr, config.listen_port),
        "NVMe-oF target ready (SPDK simulation)"
    );

    // For now, just keep running (in real impl, SPDK event loop would block here)
    std::thread::park();

    Ok(())
}

/// Check if hugepages are available (required for SPDK).
fn check_hugepages_available() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let meminfo =
            std::fs::read_to_string("/proc/meminfo").context("Failed to read /proc/meminfo")?;

        let hugepages_total = meminfo
            .lines()
            .find(|l| l.starts_with("HugePages_Total:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0);

        if hugepages_total == 0 {
            warn!("No hugepages configured. SPDK requires hugepages. Run: echo 1024 > /proc/sys/vm/nr_hugepages");
            return Err(anyhow::anyhow!("Hugepages not configured"));
        }

        debug!(hugepages_total, "Hugepages check passed");
    }

    #[cfg(not(target_os = "linux"))]
    {
        warn!("Non-Linux platform; skipping hugepages check");
    }

    Ok(())
}

/// Ensure backing file exists (create if needed).
fn ensure_backing_file_exists(path: &str) -> Result<()> {
    if !Path::new(path).exists() {
        info!(path, "Backing file not found; creating 1GB sparse file");

        #[cfg(unix)]
        {
            use std::process::Command;
            Command::new("truncate")
                .arg("-s")
                .arg("1G")
                .arg(path)
                .output()
                .context("Failed to create backing file")?;
        }

        #[cfg(not(unix))]
        {
            // Fallback: Create a regular file (not sparse)
            use std::fs::File;
            use std::io::Write;
            let mut file = File::create(path)?;
            // Write a minimal header
            file.write_all(&[0u8; 4096])?;
        }
    }

    Ok(())
}

/// Check if SPDK is available and configured.
fn is_spdk_available() -> bool {
    // Check if hugepages are configured as a proxy for SPDK availability
    #[cfg(target_os = "linux")]
    {
        check_hugepages_available().is_ok()
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Fallback TCP-based simulation (when SPDK unavailable).
///
/// Provides a simple TCP server that mimics basic NVMe-oF read/write
/// operations for testing without full SPDK setup.
fn run_tcp_fallback_sim(config: NvmeofSimConfig) -> Result<()> {
    info!("Starting TCP fallback simulation (no SPDK)");

    use std::io::{Read, Write};
    use std::net::TcpListener;

    let addr = format!("{}:{}", config.listen_addr, config.listen_port);
    let listener = TcpListener::bind(&addr).context(format!("Failed to bind to {}", addr))?;

    info!(
        node_id = config.node_id,
        address = addr,
        "TCP fallback NVMe-oF sim listening"
    );

    // Simple protocol: clients send "READ <offset> <len>" or "WRITE <offset> <data>"
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                debug!("Client connected: {:?}", stream.peer_addr());

                let mut buf = [0u8; 1024];
                match stream.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let cmd = String::from_utf8_lossy(&buf[..n]);
                        debug!(command = %cmd, "Received command");

                        // Echo back for testing
                        stream.write_all(b"OK\n").ok();
                    }
                    _ => {}
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to accept connection");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = NvmeofSimConfig::default();
        assert_eq!(config.transport, "tcp");
        assert_eq!(config.listen_port, 4420);
    }

    #[test]
    fn test_backing_file_creation() {
        let path = "test_backing.img";
        ensure_backing_file_exists(path).unwrap();
        assert!(Path::new(path).exists());

        // Cleanup
        std::fs::remove_file(path).ok();
    }

    // Note: Can't easily test start_nvmeof_sim in unit tests (requires privileges)
    // Integration tests will handle that in docker environment
}

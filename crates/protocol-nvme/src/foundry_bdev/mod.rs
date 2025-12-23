//! NVMe-oF binding for Foundry volumes (Milestone 8.2).
//!
//! This module implements the "Network Exposure" layer that wraps Foundry's
//! `VolumeBackend` in an SPDK-based NVMe-oF target. This allows any Linux
//! kernel to mount a Foundry volume as a local NVMe disk over TCP/IP.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────┐
//! │ Linux Initiator │  (nvme connect -t tcp ...)
//! └────────┬────────┘
//!          │ TCP/IP
//!          ▼
//! ┌─────────────────┐
//! │  SPDK NVMe-oF   │  (C library, polling reactor)
//! │     Target      │
//! └────────┬────────┘
//!          │
//!          ▼
//! ┌─────────────────┐
//! │  Foundry Bdev   │  (This crate: Rust FFI bridge)
//! │   (bridge.rs)   │
//! └────────┬────────┘
//!          │ Async I/O
//!          ▼
//! ┌─────────────────┐
//! │     Tokio       │  (Async runtime)
//! │    Runtime      │
//! └────────┬────────┘
//!          │
//!          ▼
//! ┌─────────────────┐
//! │     Foundry     │  (VolumeBackend trait)
//! │  VolumeBackend  │
//! └─────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```no_run
//! use foundry::{Foundry, VolumeId};
//! use protocol_nvme::foundry_bdev;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let foundry = Foundry::new();
//! let volume_id = VolumeId::new();
//! let volume = foundry.create_volume(volume_id, 100 * 1024 * 1024, None).await?;
//!
//! // Start NVMe-oF target exposing this volume
//! foundry_bdev::start_nvme_target(volume, "vol-1", 4420).await?;
//! # Ok(())
//! # }
//! ```

pub mod bdev;
pub mod bridge;

use anyhow::Result;
use foundry::VolumeBackend;
use std::sync::Arc;

/// Start an NVMe-oF target exposing the given Foundry volume.
///
/// This function:
///
/// 1. Initializes the SPDK environment (if not already initialized)
/// 2. Creates the Foundry bdev with async bridge
/// 3. Creates an NVMe-oF subsystem with the specified NQN
/// 4. Starts a TCP listener on the specified port
///
/// # Arguments
///
/// * `volume` - The Foundry volume to expose
/// * `volume_name` - Logical name for the volume (used in NQN)
/// * `port` - TCP port for NVMe-oF (default: 4420)
///
/// # NQN Format
///
/// The NVMe Qualified Name follows the format:
/// `nqn.2024-01.io.space:<volume_name>`
///
/// # Example
///
/// ```no_run
/// # use foundry::{Foundry, VolumeId};
/// # use protocol_nvme::foundry_bdev;
/// # async fn example() -> anyhow::Result<()> {
/// let foundry = Foundry::new();
/// let volume = foundry.create_volume(VolumeId::new(), 1024*1024*100, None).await?;
/// foundry_bdev::start_nvme_target(volume, "vol-1", 4420).await?;
/// # Ok(())
/// # }
/// ```
///
/// The volume can then be connected from a Linux initiator:
///
/// ```bash
/// sudo nvme connect -t tcp -n nqn.2024-01.io.space:vol-1 -a 127.0.0.1 -s 4420
/// ```
pub async fn start_nvme_target(
    volume: Arc<dyn VolumeBackend>,
    volume_name: &str,
    port: u16,
) -> Result<()> {
    tracing::info!(
        volume_name,
        port,
        "Starting NVMe-oF target for Foundry volume"
    );

    // Initialize SPDK environment
    // Note: In a real implementation, this would call spdk_env_init() and
    // start the SPDK reactor thread. For now, this is a stub.
    //
    // SPDK initialization typically looks like:
    // - Parse SPDK config file (JSON)
    // - Initialize DPDK (if using kernel bypass)
    // - Start reactor on a dedicated CPU core
    // - Initialize NVMe-oF subsystem
    unsafe {
        tracing::warn!("SPDK initialization is not yet implemented (requires C integration)");

        // This would be the real initialization sequence:
        // 1. spdk_env_opts_init(&opts)
        // 2. spdk_env_init(&opts)
        // 3. spdk_thread_create("nvmf_thread", ...)
        // 4. bdev::init_foundry_bdev(volume)
        // 5. Create NVMe-oF subsystem and listener

        // For now, just initialize the bridge (requires Tokio runtime)
        bdev::init_foundry_bdev(volume);
    }

    // Create NVMe-oF subsystem
    let nqn = format!("nqn.2024-01.io.space:{}", volume_name);
    tracing::info!(nqn, "Creating NVMe-oF subsystem");

    // Note: This would call SPDK functions like:
    // - spdk_nvmf_subsystem_create()
    // - spdk_nvmf_subsystem_add_ns() (namespace = our bdev)
    // - spdk_nvmf_subsystem_add_listener() (TCP on specified port)

    tracing::info!(nqn, port, "NVMe-oF target started (stub implementation)");

    Ok(())
}

/// Stop the NVMe-oF target and cleanup resources.
///
/// This should be called when shutting down the service.
pub async fn stop_nvme_target() -> Result<()> {
    tracing::info!("Stopping NVMe-oF target");

    unsafe {
        bdev::shutdown_foundry_bdev();
    }

    // Additional SPDK cleanup would go here:
    // - spdk_nvmf_subsystem_destroy()
    // - spdk_thread_exit()
    // - spdk_env_fini()

    Ok(())
}

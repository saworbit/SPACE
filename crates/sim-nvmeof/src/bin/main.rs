//! Standalone binary for running NVMe-oF simulation.
//!
//! This binary is the entrypoint for the "sim" container's NVMe-oF module.
//! It reads configuration from environment variables and starts the simulation.
//!
//! # Environment Variables
//!
//! - `NODE_ID`: Unique node identifier (default: "sim-node1")
//! - `BACKING_PATH`: Path to backing image (default: "/sim/nvmeof/backing.img")
//! - `TRANSPORT`: Transport type - tcp or rdma (default: "tcp")
//! - `LISTEN_ADDR`: Listen address (default: "0.0.0.0")
//! - `LISTEN_PORT`: Listen port (default: "4420")
//!
//! # Example
//!
//! ```bash
//! NODE_ID=node1 BACKING_PATH=/data/backing.img sim-nvmeof
//! ```

use anyhow::Result;
use sim_nvmeof::{start_nvmeof_sim_with_config, NvmeofSimConfig};
use std::env;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

fn main() -> Result<()> {
    // Initialize tracing
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    info!("Starting NVMe-oF simulation binary");

    // Read config from environment
    let mut config = NvmeofSimConfig::default();
    config.node_id = env::var("NODE_ID").unwrap_or(config.node_id);
    config.backing_path = env::var("BACKING_PATH").unwrap_or(config.backing_path);
    config.listen_addr = env::var("LISTEN_ADDR").unwrap_or(config.listen_addr);
    config.listen_port = env::var("LISTEN_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(config.listen_port);
    config.subsystem_nqn = env::var("SUBSYSTEM_NQN").unwrap_or(config.subsystem_nqn);

    info!(?config, "Configuration loaded from environment");

    // Start simulation (blocks until shutdown)
    match start_nvmeof_sim_with_config(config) {
        Ok(()) => {
            info!("NVMe-oF simulation exited cleanly");
            Ok(())
        }
        Err(e) => {
            error!(error = %e, "NVMe-oF simulation failed");
            Err(e)
        }
    }
}

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
use tracing::{info, error};
use tracing_subscriber::{fmt, EnvFilter};

fn main() -> Result<()> {
    // Initialize tracing
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!("Starting NVMe-oF simulation binary");

    // Read config from environment
    let config = NvmeofSimConfig {
        node_id: env::var("NODE_ID").unwrap_or_else(|_| "sim-node1".to_string()),
        backing_path: env::var("BACKING_PATH")
            .unwrap_or_else(|_| "/sim/nvmeof/backing.img".to_string()),
        transport: env::var("TRANSPORT").unwrap_or_else(|_| "tcp".to_string()),
        listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string()),
        listen_port: env::var("LISTEN_PORT")
            .unwrap_or_else(|_| "4420".to_string())
            .parse()
            .unwrap_or(4420),
        subsystem_nqn: env::var("SUBSYSTEM_NQN")
            .unwrap_or_else(|_| "nqn.2024-01.dev.adaptive-storage:space-sim".to_string()),
    };

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

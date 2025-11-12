# SPACE Simulations Guide

This document describes SPACE's simulation capabilities for testing data management features without physical hardware. Simulations enable end-to-end validation of compression, deduplication, encryption, and Phase 4 protocol views in development and CI environments.

## Table of Contents

- [Overview](#overview)
- [Simulation Modules](#simulation-modules)
  - [NVRAM Simulation](#nvram-simulation)
  - [NVMe-oF Simulation](#nvme-of-simulation)
  - [Other Simulations](#other-simulations)
- [Architecture](#architecture)
- [Usage](#usage)
  - [Quick Start](#quick-start)
  - [Selective Module Loading](#selective-module-loading)
  - [Integration with Pipeline](#integration-with-pipeline)
- [Testing](#testing)
- [Troubleshooting](#troubleshooting)
- [Future Extensions](#future-extensions)

## Overview

SPACE simulations provide:

- **Hardware Independence**: Test full data pipelines without NVMe, NVRAM, or fabric hardware
- **Modularity**: Enable/disable specific simulations (e.g., skip NVMe-oF to reduce overhead)
- **Isolation**: Simulations run in separate crates and containers, preventing production contamination
- **Realism**: Leverage SPDK for NVMe-oF protocol emulation where available

### Design Principles

1. **Separate Crates**: Each simulation is a standalone crate (`sim-nvram`, `sim-nvmeof`, `sim-other`)
2. **Optional**: Production builds exclude simulations entirely via workspace configuration
3. **Runtime Selection**: The "sim" container loads only requested modules via `SIM_MODULES` env var

## Simulation Modules

### NVRAM Simulation

**Crate**: `crates/sim-nvram`
**Purpose**: Lightweight append-only log emulation for testing write pipeline
**Dependencies**: `nvram-sim` (core implementation)

#### Features

- **File-Backed**: Persists segments to disk for multi-run tests
- **Transaction Support**: Atomic multi-segment writes
- **Metadata Tracking**: Stores compression, dedup, encryption metadata per segment
- **Fault Injection** (future): Simulate I/O errors for resilience testing

#### API Example

```rust
use sim_nvram::start_nvram_sim;
use common::SegmentId;

let log = start_nvram_sim("test.log")?;
let seg_id = SegmentId(1);

// Write
log.append(seg_id, b"test data")?;

// Read
let data = log.read(seg_id)?;
assert_eq!(data, b"test data");
```

#### Configuration

```rust
use sim_nvram::{start_nvram_sim_with_config, NvramSimConfig};

let config = NvramSimConfig {
    backing_path: "/tmp/sim_nvram.log".to_string(),
    enable_fault_injection: true,  // Enable error injection
    simulated_latency_us: 100,     // Simulate 100μs latency
};

let log = start_nvram_sim_with_config(config)?;
```

#### Performance

- **Overhead**: ~5% vs. real NVRAM (file I/O)
- **Throughput**: Suitable for up to 10K segments in tests
- **Scalability**: Single-threaded; use multiple logs for parallel tests

### NVMe-oF Simulation

**Crate**: `crates/sim-nvmeof`
**Purpose**: Heavyweight NVMe-over-Fabrics protocol emulation
**Dependencies**: `spdk-rs` (SPDK bindings)

#### Features

- **SPDK-Based**: Uses SPDK's nvmf target when available
- **TCP Fallback**: Simple TCP server when SPDK/hugepages unavailable
- **Multi-Node**: Simulate federated capsule mesh

#### Requirements

**Linux Only** (SPDK limitation):
- Hugepages enabled: `echo 1024 > /proc/sys/vm/nr_hugepages`
- Privileged container or capabilities: `--cap-add=SYS_ADMIN`

**Non-Linux**: Falls back to TCP mode (limited functionality)

#### Usage

**Standalone Binary**:
```bash
NODE_ID=node1 \
BACKING_PATH=/data/backing.img \
TRANSPORT=tcp \
LISTEN_ADDR=0.0.0.0 \
LISTEN_PORT=4420 \
  sim-nvmeof
```

**In Docker** (see [Containerization](#containerization)):
```yaml
services:
  sim:
    image: space-sim:latest
    environment:
      SIM_MODULES: nvmeof
      NODE_ID: sim-node1
    ports:
      - "4420:4420"
```

**Client Connection** (requires `nvme-cli`):
```bash
# Discover target
nvme discover -t tcp -a 127.0.0.1 -s 4420

# Connect
nvme connect -t tcp -n nqn.2024-01.dev.adaptive-storage:space-sim -a 127.0.0.1 -s 4420
```

#### Troubleshooting

- **"No hugepages configured"**: Run `echo 1024 > /proc/sys/vm/nr_hugepages` as root
- **"SPDK not available"**: Falls back to TCP mode; full SPDK requires Linux + hugepages
- **Port already in use**: Change `LISTEN_PORT` or stop conflicting service

### Other Simulations

**Crate**: `crates/sim-other`
**Purpose**: Placeholder for future simulation modules

#### Planned Extensions

1. **GPU Offload** (`--features gpu-offload`): Mock CUDA/OpenCL for CapsuleFlow testing
2. **ZNS SSD**: Simulate zoned namespaces for append-only workloads
3. **Network Conditions**: Inject latency/packet loss for mesh testing
4. **DPU/SmartNIC**: Simulate hardware accelerators

#### Adding a New Simulation

1. Add feature to `sim-other/Cargo.toml`:
   ```toml
   [features]
   gpu-offload = []
   ```

2. Implement in `sim-other/src/gpu.rs`:
   ```rust
   #[cfg(feature = "gpu-offload")]
   pub fn start_gpu_offload_sim() -> Result<()> {
       // Mock GPU compression/dedup
       Ok(())
   }
   ```

3. Update `scripts/sim-entrypoint.sh` to recognize `SIM_MODULES=gpu`

## Architecture

### Layered Design

```
┌─────────────────────────────────────────────────────────────┐
│  Application Layer (spacectl, protocol views)               │
├─────────────────────────────────────────────────────────────┤
│  Pipeline Layer (compression, dedup, encryption)            │
├─────────────────────────────────────────────────────────────┤
│  NVRAM Layer                                                 │
│  ├─ Production: nvram-sim (core)                           │
│  └─ Dev/Test: sim-nvram (wrapper with hooks)               │
├─────────────────────────────────────────────────────────────┤
│  Protocol Layer (Phase 4)                                    │
│  ├─ Production: Real NVMe-oF via SPDK                      │
│  └─ Dev/Test: sim-nvmeof (SPDK target or TCP fallback)     │
└─────────────────────────────────────────────────────────────┘
```

### Container Architecture

```
docker-compose.yml
├─ spacectl: CLI + S3 server
├─ io-engine-1, io-engine-2: Data pipeline nodes
├─ metadata-mesh: Capsule registry
└─ sim: Orchestrates simulations
   ├─ /sim/nvram: NVRAM log files
   ├─ /sim/nvmeof: NVMe-oF backing image + binary
   └─ entrypoint.sh: Parses SIM_MODULES, starts selected sims
```

## Usage

### Quick Start

1. **Build and Start Environment**:
   ```bash
   ./scripts/setup_home_lab_sim.sh
   ```

   This:
   - Builds Docker images (`space-core`, `space-sim`)
   - Configures hugepages (if Linux)
   - Starts Docker Compose with default simulations (NVRAM)

2. **Run Tests**:
   ```bash
   ./scripts/test_e2e_sim.sh
   ```

3. **Stop**:
   ```bash
   docker compose down
   ```

### Selective Module Loading

**Enable only NVRAM** (lightweight, <1GB RAM):
```bash
export SIM_MODULES=nvram
docker compose up -d
```

**Enable NVRAM + NVMe-oF** (requires ~4GB RAM, Linux):
```bash
export SIM_MODULES=nvram,nvmeof
docker compose up -d
```

**Disable all simulations** (production-like):
```bash
docker compose up -d spacectl metadata-mesh io-engine-1
# Omit "sim" service
```

### Integration with Pipeline

#### In Tests

```rust
// crates/capsule-registry/tests/my_test.rs
use sim_nvram::start_nvram_sim;

#[test]
fn test_with_sim() -> Result<()> {
    let log = start_nvram_sim("test.log")?;
    // Use log in pipeline tests
    Ok(())
}
```

#### Environment Variable Hook

Set `SPACE_SIM_MODE=1` to make pipeline use simulations at runtime:

```rust
// In pipeline.rs (conditional compilation)
#[cfg(test)]
use sim_nvram::start_nvram_sim;

fn get_nvram_log() -> Result<NvramLog> {
    if std::env::var("SPACE_SIM_MODE").is_ok() {
        start_nvram_sim("sim_nvram.log")
    } else {
        NvramLog::open("/dev/nvram") // Production path
    }
}
```

## Testing

### Unit Tests

```bash
# Test sim-nvram
cargo test -p sim-nvram --lib

# Test sim-nvmeof (may fail on non-Linux)
cargo test -p sim-nvmeof --lib
```

### Integration Tests

```bash
# Pipeline with NVRAM sim
cargo test -p capsule-registry --test pipeline_sim_integration

# All integration tests
cargo test --workspace --tests
```

### E2E Tests

```bash
# Native (no Docker)
./scripts/test_e2e_sim.sh --native

# Docker environment
./scripts/test_e2e_sim.sh

# Verbose logging
./scripts/test_e2e_sim.sh --verbose
```

## Troubleshooting

### Common Issues

**Issue**: `cargo test -p sim-nvmeof` fails with "SPDK not available"
**Solution**: Expected on non-Linux. SPDK requires hugepages. Use TCP fallback for basic tests.

**Issue**: Docker container "sim" exits immediately
**Solution**: Check logs: `docker compose logs sim`. Ensure `SIM_MODULES` is set (default: `nvram`).

**Issue**: Tests fail with "Segment not found"
**Solution**: Cleanup stale test files: `rm -f test_*.log test_*.log.segments`

**Issue**: NVMe-oF target not discoverable
**Solution**:
1. Check hugepages: `cat /proc/meminfo | grep Huge`
2. Ensure container has `--privileged` or `--cap-add=SYS_ADMIN`
3. Check logs: `docker compose logs sim | grep NVMe`

### Debug Logging

Enable debug logs:
```bash
export RUST_LOG=debug
cargo test -p sim-nvram -- --nocapture
```

Or in Docker:
```bash
docker compose up -d
docker compose logs -f sim
```

### Performance Profiling

Benchmark NVRAM sim overhead:
```bash
cargo bench -p capsule-registry -- pipeline
# Compare "real NVRAM" vs "sim-nvram" throughput
```

## Future Extensions

### Planned Features

1. **Fault Injection API**: Inject I/O errors, latency spikes, partial writes
   ```rust
   config.fault_injection = FaultConfig {
       error_rate: 0.01,  // 1% error rate
       error_types: vec![ErrorType::Timeout, ErrorType::Corruption],
   };
   ```

2. **Distributed Simulation**: Multi-node NVRAM sync for mesh testing
3. **GPU Offload Sim**: Mock CUDA kernels for CapsuleFlow
4. **Telemetry**: Export Prometheus metrics from sims
5. **Record/Replay**: Capture real workloads, replay in sim

### Contributing

To add a new simulation module:

1. Create crate: `crates/sim-<name>`
2. Add to workspace: `Cargo.toml`
3. Implement `start_<name>_sim() -> Result<()>` API
4. Update `scripts/sim-entrypoint.sh` to parse `SIM_MODULES=<name>`
5. Add tests: `cargo test -p sim-<name>`
6. Document in this file

## See Also

- [CONTAINERIZATION.md](CONTAINERIZATION.md): Docker setup details
- [architecture.md](architecture.md): SPACE system design
- [phase4.md](phase4.md): NVMe-oF protocol views
- [README.md](../README.md): Main project documentation

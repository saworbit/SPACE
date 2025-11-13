# SPACE Simulation Runtime Directory

This directory contains runtime configuration and state for SPACE simulations.
It's mounted into the Docker `sim` container at `/sim`.

## Directory Structure

```
sim/
├── entrypoint.sh       # Container entrypoint (selective module loading)
├── nvram/              # NVRAM simulation state
│   ├── config.toml     # NVRAM configuration
│   └── *.log           # Simulation log files (runtime generated)
├── nvmeof/             # NVMe-oF simulation state
│   ├── config.toml     # NVMe-oF configuration
│   └── backing.img     # Backing storage (runtime generated)
└── other/              # Future simulation modules
    └── README.md       # Placeholder

## Configuration

Each simulation module has a `config.toml` file for runtime behavior:

- **nvram/config.toml**: NVRAM log simulation settings (latency, fault injection)
- **nvmeof/config.toml**: NVMe-oF fabric settings (transport, SPDK mode)

## Docker Volume Mapping

In `docker-compose.yml`, this directory is mounted:

```yaml
volumes:
  - ./sim:/sim:rw
```

This allows:
1. Configuration from host
2. Runtime state persistence
3. Easy debugging (inspect logs on host)

## Selective Module Loading

Control which simulations run via `SIM_MODULES` environment variable:

```bash
# Only NVRAM (lightweight)
docker compose up -d sim -e SIM_MODULES=nvram

# NVRAM + NVMe-oF (full stack)
docker compose up -d sim -e SIM_MODULES=nvram,nvmeof

# All modules
docker compose up -d sim -e SIM_MODULES=nvram,nvmeof,other
```

## Cleaning Simulation State

To reset simulations (clear logs, backing files):

```bash
# Stop containers first
docker compose down

# Remove runtime files
rm -f sim/nvram/*.log sim/nvram/*.segments
rm -f sim/nvmeof/backing.img

# Restart
docker compose up -d
```

## See Also

- [SIMULATIONS.md](../docs/SIMULATIONS.md): Full simulation guide
- [CONTAINERIZATION.md](../docs/CONTAINERIZATION.md): Docker architecture
- [scripts/setup_home_lab_sim.sh](../scripts/setup_home_lab_sim.sh): Automated setup

# SPACE Containerization Guide

This document describes SPACE's Docker-based deployment strategy for microservices architecture, development environments, and simulation testing.

## Table of Contents

- [Overview](#overview)
- [Docker Images](#docker-images)
  - [Core Image](#core-image)
  - [Simulation Image](#simulation-image)
- [Docker Compose Setup](#docker-compose-setup)
- [Services](#services)
- [Networking](#networking)
- [Volumes and Persistence](#volumes-and-persistence)
- [Security Considerations](#security-considerations)
- [Production Deployment](#production-deployment)
- [Troubleshooting](#troubleshooting)

## Overview

SPACE uses Docker containers to:

- **Isolate Components**: Separate spacectl, io-engine, metadata-mesh, and simulations
- **Enable Microservices**: Scale io-engine nodes independently
- **Facilitate Testing**: Run simulations without affecting production builds
- **Simplify Deployment**: Single `docker compose up` for local development

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Docker Compose Network                        │
│                                                                   │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │   spacectl   │  │ io-engine-1  │  │ io-engine-2  │          │
│  │  (S3 API)    │  │  (Pipeline)  │  │  (Pipeline)  │          │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘          │
│         │                  │                  │                  │
│         └──────────────────┼──────────────────┘                  │
│                            │                                     │
│                   ┌────────▼────────┐                            │
│                   │ metadata-mesh   │                            │
│                   │ (Raft KV store) │                            │
│                   └─────────────────┘                            │
│                                                                   │
│  ┌──────────────────────────────────────────────┐               │
│  │  sim (dev/test only)                         │               │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  │               │
│  │  │  nvram   │  │  nvmeof  │  │  other   │  │               │
│  │  └──────────┘  └──────────┘  └──────────┘  │               │
│  └──────────────────────────────────────────────┘               │
└─────────────────────────────────────────────────────────────────┘
```

## Docker Images

### Core Image

**Dockerfile**: [`Dockerfile`](../Dockerfile)
**Image Name**: `space-core:latest`
**Purpose**: Production components (spacectl, io-engine, metadata-mesh)

#### Build

```bash
docker build -t space-core:latest .
```

#### What's Included

- **Binaries**:
  - `spacectl`: CLI and S3 server (from `protocol-s3`)
  - Additional binaries for io-engine, metadata-mesh (if separate)
- **Runtime**: Ubuntu 24.04 with minimal dependencies (ca-certificates, libssl3)
- **User**: Non-root user `space` (UID 1000)
- **Excluded**: All simulation crates (`sim-*`)

#### Dockerfile Highlights

```dockerfile
# Multi-stage build for minimal image size
FROM rust:1.78 as builder
RUN cargo build --release \
    --workspace \
    --exclude sim-nvram \
    --exclude sim-nvmeof \
    --exclude sim-other

FROM ubuntu:24.04
COPY --from=builder /usr/src/space/target/release/spacectl /usr/local/bin/
```

#### Configuration

- **Default CMD**: `spacectl --help` (override with `docker run` args)
- **Health Check**: Uncomment S3 server health check if running protocol-s3
- **Environment**: Set `RUST_LOG`, `SPACE_CONFIG_PATH` as needed

### Simulation Image

**Dockerfile**: [`Dockerfile.sim`](../Dockerfile.sim)
**Image Name**: `space-sim:latest`
**Purpose**: Dev/test simulations (NVRAM, NVMe-oF, etc.)

#### Build

```bash
docker build -t space-sim:latest -f Dockerfile.sim .
```

#### What's Included

- **Binaries**:
  - `sim-nvmeof`: NVMe-oF simulation binary
  - (sim-nvram is library-only, no standalone binary)
- **Runtime**: Ubuntu 24.04 + simulation tools (numactl, pciutils, iproute2)
- **Entrypoint**: `scripts/sim-entrypoint.sh` (selective module loading)
- **Privileged**: Requires `--privileged` for SPDK hugepages

#### Configuration

- **Environment Variables**:
  - `SIM_MODULES`: Comma-separated list (e.g., `nvram,nvmeof`)
  - `NODE_ID`: Node identifier (default: `sim-node1`)
  - `RUST_LOG`: Log level (default: `info`)
- **Volumes**: `/sim` directory for backing files

## Docker Compose Setup

**File**: [`docker-compose.yml`](../docker-compose.yml)

### Quick Start

```bash
# Build images (first time or after code changes)
docker build -t space-core:latest .
docker build -t space-sim:latest -f Dockerfile.sim .

# Start all services
docker compose up -d

# View logs
docker compose logs -f

# Stop
docker compose down
```

### Configuration

```yaml
services:
  spacectl:
    image: space-core:latest
    ports:
      - "8080:8080"  # S3 API
    volumes:
      - spacectl-data:/data
    environment:
      RUST_LOG: info

  io-engine-1:
    image: space-core:latest
    volumes:
      - io-engine-1-data:/data
    environment:
      NODE_ID: io-engine-1

  metadata-mesh:
    image: space-core:latest
    volumes:
      - metadata-data:/data

  sim:
    image: space-sim:latest
    privileged: true  # Required for hugepages
    ports:
      - "4420:4420"  # NVMe-oF
    volumes:
      - sim-data:/sim
    environment:
      SIM_MODULES: nvram,nvmeof
```

## Services

### spacectl

**Purpose**: CLI tool and S3 protocol view
**Ports**: 8080 (S3 API)
**Dependencies**: metadata-mesh, io-engine-1
**Command**: Override default `spacectl --help` with custom commands

**Example Override**:
```bash
docker compose run spacectl spacectl create my-capsule input.txt
```

### io-engine-1, io-engine-2

**Purpose**: Data pipeline nodes (compression, dedup, encryption)
**Scalability**: Add more nodes in `docker-compose.yml` for multi-node tests
**Environment**: Set `NODE_ID` to unique identifiers

**Scaling**:
```yaml
services:
  io-engine:
    image: space-core:latest
    deploy:
      replicas: 3  # Docker Swarm (or use docker-compose scale)
```

### metadata-mesh

**Purpose**: Raft-based KV store for capsule registry
**Persistence**: Stores capsule metadata
**Dependencies**: None (base service)

**Note**: Current placeholder; integrate with `capsule-registry` Raft implementation.

### sim (Dev/Test Only)

**Purpose**: Orchestrates simulation modules
**Privileged**: Yes (for SPDK/hugepages)
**Selective Loading**: Set `SIM_MODULES` environment variable

**Disable in Production**:
```yaml
# Option 1: Use profiles (Docker Compose v2.20+)
services:
  sim:
    profiles: ["dev"]  # Only starts with --profile dev

# Option 2: Comment out service
# sim: ...
```

## Networking

### Default Bridge Network

Docker Compose creates a bridge network `space-net` where all services can communicate by service name:

```bash
# From spacectl container:
curl http://metadata-mesh:8080/health
ping io-engine-1
```

### Port Mapping

| Service  | Internal Port | External Port | Purpose                |
|----------|---------------|---------------|------------------------|
| spacectl | 8080          | 8080          | S3 API                 |
| sim      | 4420          | 4420          | NVMe-oF target (TCP)   |

### Custom Networks

For advanced setups (e.g., separate control/data planes):

```yaml
networks:
  control-plane:
    driver: bridge
  data-plane:
    driver: bridge

services:
  spacectl:
    networks:
      - control-plane
  io-engine-1:
    networks:
      - control-plane
      - data-plane
```

## Volumes and Persistence

### Named Volumes

Docker Compose creates named volumes for persistent data:

- `metadata-data`: Capsule metadata (Raft log)
- `io-engine-1-data`, `io-engine-2-data`: NVRAM logs, segment data
- `spacectl-data`: CLI state, temp files
- `sim-data`: Simulation backing files

### Inspect Volumes

```bash
# List volumes
docker volume ls

# Inspect
docker volume inspect space_metadata-data

# Backup
docker run --rm -v space_metadata-data:/data -v $(pwd):/backup \
  ubuntu tar czf /backup/metadata-backup.tar.gz /data
```

### Cleanup

```bash
# Remove volumes (WARNING: deletes data)
docker compose down -v
```

## Security Considerations

### Development vs. Production

| Aspect                | Development           | Production                     |
|-----------------------|-----------------------|--------------------------------|
| Privileged Containers | `sim` service only    | None (use capabilities)        |
| User                  | `root` for sim        | Non-root for all               |
| Secrets               | Environment variables | Docker secrets, Vault          |
| Network               | Bridge                | Encrypted overlay (swarm/k8s)  |
| TLS                   | Optional              | Required (mTLS)                |

### Production Hardening

1. **Remove `privileged: true`**: Use specific capabilities:
   ```yaml
   cap_add:
     - SYS_ADMIN     # For hugepages (sim only)
     - NET_ADMIN     # For network namespaces
   ```

2. **Enable AppArmor/SELinux**: Apply security profiles

3. **Use Docker Secrets**:
   ```yaml
   services:
     spacectl:
       secrets:
         - encryption_key
   secrets:
     encryption_key:
       file: ./secrets/encryption_key.txt
   ```

4. **Enable TLS**: Configure mTLS between services

### Simulation Isolation

**Critical**: `sim` container is privileged and dev-only. Never deploy to production.

- Use Docker profiles or separate Compose files
- CI/CD: Different pipelines for dev (with sim) vs. prod (without)

## Production Deployment

### Docker Swarm

```bash
# Initialize swarm
docker swarm init

# Deploy stack
docker stack deploy -c docker-compose.yml space

# Scale io-engine
docker service scale space_io-engine=5
```

### Kubernetes

For Kubernetes deployment, see [`deployment/`](../deployment/) directory:

- `deployment/space-core-deployment.yaml`
- `deployment/csi-deployment.yaml` (Phase 4 CSI driver)

**Helm Chart** (future):
```bash
helm install space ./deployment/helm/space-chart
```

### Environment-Specific Configs

Use `.env` files:

```bash
# docker-compose.prod.yml
services:
  spacectl:
    env_file:
      - .env.prod
```

```bash
# .env.prod
RUST_LOG=warn
SPACE_CONFIG_PATH=/config/production.yaml
```

## Troubleshooting

### Common Issues

**Issue**: `docker compose up` fails with "image not found"
**Solution**: Build images first: `docker build -t space-core:latest .`

**Issue**: `sim` container exits immediately
**Solution**: Check logs: `docker compose logs sim`. Ensure `SIM_MODULES` is set.

**Issue**: Services can't communicate (e.g., `spacectl` can't reach `metadata-mesh`)
**Solution**: Verify network: `docker network inspect space_space-net`. Check service names in code.

**Issue**: Volumes not persisting
**Solution**: Use named volumes (not bind mounts) in `docker-compose.yml`. Check with `docker volume ls`.

**Issue**: Permission denied in containers
**Solution**: Ensure `chown -R space:space /data` in Dockerfile for non-root user.

### Debugging

**Enter running container**:
```bash
docker compose exec spacectl sh
# Inside container:
spacectl --version
ps aux
```

**View container resource usage**:
```bash
docker stats
```

**Inspect network**:
```bash
docker network inspect space_space-net
```

**Check logs with timestamps**:
```bash
docker compose logs -f --timestamps spacectl
```

### Performance Tuning

**Increase memory limit**:
```yaml
services:
  io-engine-1:
    deploy:
      resources:
        limits:
          memory: 4G
```

**Use tmpfs for temp data**:
```yaml
services:
  io-engine-1:
    tmpfs:
      - /tmp:rw,noexec,nosuid,size=1g
```

## See Also

- [SIMULATIONS.md](SIMULATIONS.md): Simulation module details
- [architecture.md](architecture.md): SPACE system design
- [README.md](../README.md): Main project documentation
- [Docker Compose Docs](https://docs.docker.com/compose/)
- [Docker Best Practices](https://docs.docker.com/develop/dev-best-practices/)

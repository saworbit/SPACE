# SPACE Containerization Guide v3.0

**One-Command Magic for Adaptive Data Ecosystems**

This guide provides comprehensive documentation for deploying SPACE using Docker containers.

## Table of Contents

- [Overview](#overview)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Container Images](#container-images)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Advanced Usage](#advanced-usage)
- [Security Considerations](#security-considerations)

---

## Overview

SPACE containerization provides:
- Production-ready multi-stage Docker builds (<180 MB images)
- One-command deployment via docker-compose
- Simulation environment for testing without hardware
- Zero-trust security posture with non-root containers
- Health checks and service dependencies

### Key Features

- **Fast Builds**: Optimized .dockerignore reduces build time by 5x
- **Multi-Stage**: Separate builder and runtime stages for minimal image size
- **Smart Entrypoint**: Route to S3, registry, or CLI based on command
- **Simulation Support**: NVMe-oF and NVRAM simulators for development
- **Production Ready**: Health checks, proper user permissions, logging

---

## Quick Start

### Prerequisites

- Docker 20.10+ or Docker Desktop
- Docker Compose v2.0+
- 2GB free disk space
- Available ports: 8080 (S3), 5000 (registry)

### 90-Second Deployment

```bash
# Clone repository
git clone https://github.com/saworbit/SPACE && cd SPACE

# Build images (first time only, ~5-10 minutes)
docker build -f containerization/Dockerfile -t space-core:latest .
docker build -f containerization/Dockerfile.sim -t space-sim:latest .

# Start cluster
docker compose -f containerization/docker-compose.yml up -d

# Test S3 endpoint
curl -X PUT --data-binary @test.txt http://localhost:8080/bucket/test.txt
curl http://localhost:8080/bucket/test.txt
```

---

## Architecture

### Container Stack

```
┌─────────────────────────────────────────────┐
│          Docker Compose Stack               │
├─────────────────────────────────────────────┤
│                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐ │
│  │ Registry │  │  Node 1  │  │   Sim    │ │
│  │  :5000   │  │  :8080   │  │          │ │
│  │  Raft    │  │   S3     │  │  NVMe-oF │ │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘ │
│       │             │             │        │
│       └─────────────┴─────────────┘        │
│                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐ │
│  │ registry │  │  node1   │  │   sim    │ │
│  │  volume  │  │  volume  │  │  volume  │ │
│  └──────────┘  └──────────┘  └──────────┘ │
└─────────────────────────────────────────────┘
```

### Image Layers

#### space-core (Production)
```
Layer 1: Ubuntu 24.04 base
Layer 2: System dependencies (ca-certificates, curl)
Layer 3: SPACE binaries (spacectl, protocol-s3, capsule-registry)
Layer 4: User setup + directories
Layer 5: Entrypoint script
Total: ~180 MB
```

#### space-sim (Simulation)
```
Layer 1: Ubuntu 24.04 base
Layer 2: System dependencies (numactl)
Layer 3: nvram-sim binary
Layer 4: Sim entrypoint
Total: ~150 MB
```

---

## Container Images

### space-core:latest

**Purpose**: Production SPACE node running S3 API, registry, or CLI

**Binaries**:
- `/usr/local/bin/spacectl` - CLI tool
- `/usr/local/bin/protocol-s3` - S3 API server
- `/usr/local/bin/capsule-registry` - Metadata registry

**Volumes**:
- `/capsules` - Capsule data and metadata
- `/logs` - Application logs

**Ports**:
- 8080 - S3 API (configurable)
- 5000 - Raft consensus (registry mode)

**User**: `space` (UID 1000, non-root)

**Commands**:
```bash
# S3 server mode
docker run -p 8080:8080 space-core:latest s3

# Registry mode
docker run -p 5000:5000 space-core:latest registry

# CLI mode
docker run space-core:latest cli create --file test.txt
```

### space-sim:latest

**Purpose**: Hardware simulation for development/testing

**Binary**:
- `/usr/local/bin/nvram-sim` - NVRAM and NVMe-oF simulator

**Volumes**:
- `/sim` - Simulation state

**Environment**:
- `SIM_MODULES` - Comma-separated modules (default: `nvram,nvmeof`)

**Capabilities**:
- `IPC_LOCK` - Hugepage allocation
- `SYS_ADMIN` - RDMA simulation (optional)

---

## Configuration

### Environment Variables

#### space-core

**Logging**:
```bash
RUST_LOG=info              # Log level (debug, info, warn, error)
SPACE_LOG_FORMAT=compact   # Log format (compact, json)
```

**Encryption**:
```bash
SPACE_MASTER_KEY=<64-hex>  # 256-bit master key
SPACE_KYBER_KEY_PATH=/capsules/kyber.key  # Post-quantum key
```

**Deduplication**:
```bash
DEDUP_ENABLED=true         # Enable content dedup
ENCRYPTION_ALGO=xts-aes-256  # Encryption algorithm
```

**S3 Configuration**:
```bash
S3_BIND_ADDR=0.0.0.0:8080  # S3 listen address
```

**Registry Configuration**:
```bash
RAFT_BIND_ADDR=0.0.0.0:5000  # Raft consensus address
SPACE_METADATA_PATH=/capsules/space.metadata
SPACE_NVRAM_PATH=/capsules/space.nvram
```

**Advanced Security** (requires `--features advanced-security`):
```bash
# Bloom filters
SPACE_BLOOM_CAPACITY=10000000
SPACE_BLOOM_FPR=0.001

# Audit log
SPACE_AUDIT_LOG=/capsules/space.audit.log
SPACE_AUDIT_FLUSH=5
SPACE_TSA_ENDPOINT=https://tsa.local/submit
SPACE_TSA_API_KEY=demo-token

# SPIFFE + mTLS
SPACE_ALLOWED_SPIFFE_IDS=spiffe://demo/client-a,spiffe://demo/client-b
SPACE_SPIFFE_ENDPOINT=ws://127.0.0.1:9001/identities
```

#### space-sim

```bash
SIM_MODULES=nvram,nvmeof   # Modules to simulate
SIM_NVRAM_SIZE=10G         # NVRAM capacity
SIM_HUGEPAGES=128          # Hugepage count
```

### Volume Management

**Named Volumes** (recommended for persistence):
```yaml
volumes:
  registry:
  node1:
  sim:
```

**Bind Mounts** (for development):
```yaml
volumes:
  - ./data/registry:/capsules
  - ./data/node1:/capsules
  - ./sim-data:/sim
```

**Backup Volumes**:
```bash
# Backup registry
docker run --rm -v registry:/data -v $(pwd):/backup \
  ubuntu tar czf /backup/registry-backup.tar.gz /data

# Restore registry
docker run --rm -v registry:/data -v $(pwd):/backup \
  ubuntu tar xzf /backup/registry-backup.tar.gz -C /
```

## Troubleshooting

### Common Issues

#### Build Failures

**Symptom**: Docker build times out or fails during cargo build

**Solutions**:
```bash
# Clean Docker build cache
docker builder prune -a

# Increase Docker memory (Docker Desktop)
# Settings → Resources → Memory → 8GB+

# Use BuildKit
export DOCKER_BUILDKIT=1
docker build -f containerization/Dockerfile -t space-core:latest .
```

#### Port Conflicts

**Symptom**: `Error: bind: address already in use`

**Solutions**:
```bash
# Check what's using port 8080
lsof -i :8080  # Linux/macOS
netstat -ano | findstr :8080  # Windows

# Stop conflicting service or change port
docker compose down
# Edit docker-compose.yml: ports: ["8081:8080"]
docker compose up -d
```

#### Hugepage Errors

**Symptom**: `Cannot allocate memory` in sim container

**Solutions**:
```bash
# Linux: Allocate hugepages
sudo sysctl vm.nr_hugepages=128

# macOS/Windows: Reduce hugepage requirements
# Edit docker-compose.yml: SIM_HUGEPAGES=0
```

#### Health Check Failures

**Symptom**: Containers restart continuously

**Solutions**:
```bash
# Check logs
docker compose logs registry
docker compose logs node1

# Verify health endpoint
docker exec -it space-node1-1 curl http://localhost:8080/health

# Disable health checks temporarily
# Edit docker-compose.yml: comment out healthcheck section
```

### Debugging

**Container Logs**:
```bash
# All services
docker compose logs -f

# Specific service
docker compose logs -f node1

# Last 100 lines
docker compose logs --tail=100 node1
```

**Shell Access**:
```bash
# Access running container
docker exec -it space-node1-1 bash

# Run one-off command
docker exec space-node1-1 spacectl --help
```

**Network Inspection**:
```bash
# List networks
docker network ls

# Inspect network
docker network inspect space_default

# Test connectivity
docker exec space-node1-1 curl http://registry:5000/health
```

---

## Advanced Usage

### Multi-Node Scaling

Scale to 3 S3 nodes:

```yaml
# docker-compose.yml
services:
  node1:
    image: space-core:latest
    command: ["s3"]
    ports: ["8080:8080"]
    volumes: [node1:/capsules]
    environment:
      - REGISTRY_ADDR=registry:5000

  node2:
    image: space-core:latest
    command: ["s3"]
    ports: ["8081:8080"]
    volumes: [node2:/capsules]
    environment:
      - REGISTRY_ADDR=registry:5000

  node3:
    image: space-core:latest
    command: ["s3"]
    ports: ["8082:8080"]
    volumes: [node3:/capsules]
    environment:
      - REGISTRY_ADDR=registry:5000

volumes: [registry, node1, node2, node3]
```

### Production Deployment

**Compose Override** (docker-compose.prod.yml):
```yaml
version: "3.9"
services:
  registry:
    restart: unless-stopped
    mem_limit: 2g
    cpu_count: 2
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

  node1:
    restart: unless-stopped
    mem_limit: 4g
    cpu_count: 4
    environment:
      - RUST_LOG=warn
      - SPACE_MASTER_KEY=${SPACE_MASTER_KEY}
```

**Deploy**:
```bash
docker compose \
  -f containerization/docker-compose.yml \
  -f docker-compose.prod.yml \
  up -d
```

### CI/CD Integration

**GitHub Actions**:
```yaml
# .github/workflows/docker-build.yml
name: Docker Build
on:
  push:
    branches: [main]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v3

    - name: Build images
      run: |
        docker build -f containerization/Dockerfile -t space-core:${{ github.sha }} .
        docker build -f containerization/Dockerfile.sim -t space-sim:${{ github.sha }} .

    - name: Run tests
      run: |
        docker compose -f containerization/docker-compose.yml up -d
        docker exec space-node1-1 spacectl --version
        docker compose down
```

---

## Security Considerations

### Container Security Best Practices

1. **Non-Root User**: All containers run as non-root (UID 1000)
2. **Read-Only Root**: Consider `--read-only` flag with tmpfs mounts
3. **Capabilities**: Minimal capabilities (sim requires IPC_LOCK)
4. **Secrets Management**: Use Docker secrets or environment files
5. **Network Isolation**: Use custom networks, not host networking

### Secrets Management

**Docker Secrets** (Swarm mode):
```yaml
services:
  node1:
    secrets:
      - space_master_key
    environment:
      - SPACE_MASTER_KEY_FILE=/run/secrets/space_master_key

secrets:
  space_master_key:
    external: true
```

**Environment Files**:
```bash
# .env file (add to .gitignore!)
SPACE_MASTER_KEY=<64-hex>
SPACE_TSA_API_KEY=<api-key>

# Use with compose
docker compose --env-file .env up -d
```

### Image Scanning

```bash
# Scan for vulnerabilities
docker scan space-core:latest

# Use Trivy
trivy image space-core:latest

# Use Grype
grype space-core:latest
```

---

## Maintenance

### Updates

```bash
# Pull latest images
docker compose pull

# Recreate containers
docker compose up -d --force-recreate
```

### Cleanup

```bash
# Remove stopped containers
docker compose down

# Remove volumes (WARNING: deletes data!)
docker compose down -v

# Clean build cache
docker builder prune -a

# Remove unused images
docker image prune -a
```

### Backup Strategy

```bash
#!/bin/bash
# backup.sh - Backup SPACE volumes

DATE=$(date +%Y%m%d-%H%M%S)
BACKUP_DIR="./backups/$DATE"

mkdir -p "$BACKUP_DIR"

# Backup each volume
for vol in registry node1 sim; do
  docker run --rm \
    -v space_$vol:/data \
    -v "$BACKUP_DIR":/backup \
    ubuntu tar czf "/backup/$vol.tar.gz" /data
done

echo "Backup complete: $BACKUP_DIR"
```

---

## References

- [Docker Documentation](https://docs.docker.com/)
- [Docker Compose Specification](https://docs.docker.com/compose/compose-file/)
- [SPACE Architecture](architecture.md)
- [SPACE Simulations Guide](SIMULATIONS.md)

---

**© 2024 SPACE Project** • [Apache 2.0 License](../LICENSE)

*Built with Rust • Deployed with Docker*

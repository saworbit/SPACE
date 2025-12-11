# SPACE Multi-Node Deployment Guide

This guide explains how to deploy and operate SPACE in multi-node mode with PODMS (Policy-Driven Object Management System) capabilities.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Prerequisites](#prerequisites)
3. [Quick Start](#quick-start)
4. [Configuration](#configuration)
5. [Monitoring & Observability](#monitoring--observability)
6. [Operations](#operations)
7. [Troubleshooting](#troubleshooting)
8. [Advanced Topics](#advanced-topics)

---

## Architecture Overview

SPACE multi-node deployment consists of several integrated components:

### Core Components

```
┌─────────────────────────────────────────────────────────────┐
│                    SPACE Multi-Node Mesh                    │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│  │  Node 1  │◄─┤  Node 2  │◄─┤  Node 3  │◄─┤  Node N  │  │
│  │  (Seed)  │─►│          │─►│          │─►│          │  │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘  │
│       │             │             │             │          │
│       └─────────────┴─────────────┴─────────────┘          │
│                     Gossip Layer                           │
│                  (libp2p + gossipsub)                      │
└─────────────────────────────────────────────────────────────┘
```

### Per-Node Components

Each node runs:

1. **PODMS Orchestrator**: Coordinates multi-node operations
   - Gossip layer for state propagation
   - Policy compiler for intelligent decisions
   - Scaling agent for autonomous actions
   - Mesh networking for replication

2. **Protocol Gateways**: Multiple access methods
   - S3-compatible REST API
   - NFS namespace facade
   - Block volume interface
   - WebUI for monitoring

3. **Storage Pipeline**: Modular processing
   - Compression (LZ4/Zstd)
   - Deduplication (BLAKE3)
   - Encryption (XTS-AES-256)
   - NVRAM persistence

### Data Flow

```
Client → S3/NFS/Block Gateway
  ↓
Capsule Pipeline (Compress/Dedup/Encrypt)
  ↓
Local NVRAM Log
  ↓
Gossip: "NewCapsule" event
  ↓
Policy Compiler: Evaluate RPO/Latency/Sovereignty
  ↓
Scaling Agent: Trigger Replication/Migration
  ↓
Mesh Network: Zero-copy replication to targets
  ↓
Remote Nodes: Receive/Dedup/Persist
```

---

## Prerequisites

### System Requirements

**Minimum (per node):**
- CPU: 4 cores
- RAM: 8 GB
- Disk: 100 GB SSD
- Network: 1 Gbps

**Recommended (per node):**
- CPU: 16+ cores
- RAM: 64 GB
- Disk: 1 TB NVMe
- Network: 10 Gbps (RDMA-capable for production)

### Software Requirements

- Docker & Docker Compose (for containerized deployment)
- Rust 1.75+ (for building from source)
- Linux kernel 5.15+ (for eBPF features)

---

## Quick Start

### Docker Compose Deployment (Development/Testing)

The fastest way to get a multi-node SPACE cluster running:

```bash
# Clone the repository
git clone https://github.com/saworbit/SPACE.git
cd space

# Start 3-node mesh with monitoring
docker-compose -f docker-compose.multi-node.yml up --build

# Access points:
# - Node 1 S3:  http://localhost:9001
# - Node 1 Web: http://localhost:8081
# - Node 2 S3:  http://localhost:9002
# - Node 2 Web: http://localhost:8082
# - Node 3 S3:  http://localhost:9003
# - Node 3 Web: http://localhost:8083
# - Prometheus: http://localhost:9090
# - Grafana:    http://localhost:3000 (admin/space)
```

### Verify Mesh Formation

```bash
# Check node 1 peers
curl http://localhost:8081/api/peers

# Check gossip stats
curl http://localhost:8081/api/gossip/stats

# Expected output:
# {
#   "connected_peers": 2,
#   "messages_sent": 150,
#   "messages_received": 300,
#   "avg_convergence_ms": 45.2
# }
```

### Test Replication

```bash
# Upload object to node 1
aws s3 --endpoint-url http://localhost:9001 cp test.dat s3://test-bucket/

# Wait for replication (check logs)
docker logs space-node-1 | grep "replication complete"

# Verify object on node 2
aws s3 --endpoint-url http://localhost:9002 ls s3://test-bucket/
```

---

## Configuration

### Orchestrator Configuration

Each node is configured via environment variables or YAML config file.

#### Environment Variables

```bash
# Node identity
SPACE_NODE_ID=node-1
SPACE_ZONE=us-west-metro

# Network
SPACE_LISTEN_ADDR=0.0.0.0:9000
SPACE_SEED_PEERS=node-1.example.com:9000,node-2.example.com:9000

# Policy defaults
SPACE_DEFAULT_POLICY=metro-sync  # or: async-batch, no-replication
SPACE_GOSSIP_FANOUT=8
SPACE_HEARTBEAT_INTERVAL_MS=1000

# Logging
RUST_LOG=info,space=debug,podms_orchestrator=debug
```

#### YAML Configuration

```yaml
# /etc/space/orchestrator.yml
node_id: "node-1"
listen_addr: "0.0.0.0:9000"
zone_name: "us-west-metro"

default_policy:
  compression: adaptive
  encryption: xts-aes-256
  deduplication: true
  rpo: 0s  # Zero-RPO metro-sync
  latency_target: 2ms
  sovereignty: zone

seed_peers:
  - "172.20.0.10:9000"
  - "172.20.0.11:9000"

gossip_fanout: 8
heartbeat_interval_ms: 1000
message_ttl: 10
max_message_size: 4096

# Signing key should be loaded from secure vault
signing_key: ${SPACE_GOSSIP_KEY}  # 32-byte hex string
```

### Policy Profiles

SPACE includes several pre-configured policy profiles:

#### Metro-Sync (Zero-RPO)

```yaml
# Synchronous replication within metro zone
compression: lz4
encryption: xts-aes-256
deduplication: true
rpo: 0s
latency_target: 2ms
sovereignty: zone
```

**Use cases:** Financial transactions, medical records, legal documents

#### Async-Batch (Low-RPO)

```yaml
# Asynchronous batched replication
compression: zstd-9
encryption: xts-aes-256
deduplication: true
rpo: 5m
latency_target: 100ms
sovereignty: global
```

**Use cases:** Media assets, backups, analytics data

#### No-Replication (Ephemeral)

```yaml
# Local-only, no replication
compression: lz4
encryption: disabled
deduplication: false
rpo: null
latency_target: 1ms
sovereignty: local
```

**Use cases:** Temporary files, build artifacts, cache

---

## Monitoring & Observability

### Metrics Exposition

Each node exposes Prometheus metrics at `/api/metrics`:

**Gossip Metrics:**
- `space_gossip_messages_sent_total`
- `space_gossip_messages_received_total`
- `space_gossip_convergence_seconds`
- `space_gossip_peers_connected`
- `space_gossip_bandwidth_bytes`

**Replication Metrics:**
- `space_replication_segments_sent_total`
- `space_replication_segments_received_total`
- `space_replication_bytes_sent`
- `space_replication_dedup_hits_total`

**Pipeline Metrics:**
- `space_capsules_created_total`
- `space_segments_compressed_total`
- `space_segments_encrypted_total`
- `space_dedup_ratio`

### Grafana Dashboards

Pre-built dashboards are available in `deploy/grafana-dashboards/`:

1. **Mesh Overview**: Cluster-wide health, peer connectivity
2. **Replication**: Bandwidth, latency, dedup efficiency
3. **Storage**: Capacity, IOPS, dedup savings
4. **Gossip**: Message rates, convergence times

### Logs

Structured JSON logs with tracing:

```bash
# Follow logs from all nodes
docker-compose -f docker-compose.multi-node.yml logs -f

# Filter for scaling agent actions
docker logs space-node-1 2>&1 | jq 'select(.target == "podms_orchestrator::scaling")'

# Search for replication events
docker logs space-node-1 2>&1 | grep "replication complete"
```

---

## Operations

### Adding a New Node

```bash
# 1. Configure new node
cat > /etc/space/orchestrator.yml <<EOF
node_id: "node-4"
listen_addr: "0.0.0.0:9000"
zone_name: "us-west-metro"
seed_peers:
  - "172.20.0.10:9000"  # Existing seed node
# ... rest of config
EOF

# 2. Start node
docker run -d \
  --name space-node-4 \
  -p 9004:9004 \
  -v /etc/space:/etc/space \
  space:latest

# 3. Verify mesh join
curl http://localhost:8084/api/peers
# Should show 3+ peers

# 4. Node will automatically:
# - Discover peers via gossip
# - Receive replicated capsules
# - Participate in rebalancing
```

### Triggering Manual Replication

```bash
# Via spacectl
spacectl replicate capsule <capsule-id> --to node-2,node-3

# Via API
curl -X POST http://localhost:8081/api/replicate \
  -H "Content-Type: application/json" \
  -d '{
    "capsule_id": "550e8400-e29b-41d4-a716-446655440000",
    "targets": ["node-2", "node-3"],
    "strategy": "metro-sync"
  }'
```

### Evacuating a Node

```bash
# Gradual evacuation (cold data first)
curl -X POST http://localhost:8081/api/telemetry \
  -H "Content-Type: application/json" \
  -d '{
    "type": "NodeDegraded",
    "node_id": "node-2",
    "reason": "maintenance"
  }'

# Immediate evacuation (parallel)
curl -X POST http://localhost:8081/api/telemetry \
  -H "Content-Type: application/json" \
  -d '{
    "type": "NodeDegraded",
    "node_id": "node-2",
    "reason": "disk_failure"
  }'
```

### Rebalancing Cluster

```bash
# Trigger rebalancing if capacity skew > 20%
curl -X POST http://localhost:8081/api/telemetry \
  -H "Content-Type: application/json" \
  -d '{
    "type": "CapacityThreshold",
    "node_id": "node-1",
    "used_bytes": 850000000000,
    "total_bytes": 1000000000000,
    "threshold_pct": 0.8
  }'

# Scaling agent will automatically migrate capsules
# to underutilized nodes
```

---

## Troubleshooting

### Peers Not Connecting

**Symptoms:** `connected_peers: 0` in gossip stats

**Diagnosis:**
```bash
# Check network connectivity
docker exec space-node-1 ping space-node-2

# Check firewall rules
sudo iptables -L | grep 9000

# Inspect gossip logs
docker logs space-node-1 2>&1 | grep "gossip"
```

**Resolution:**
- Verify `SPACE_SEED_PEERS` is set correctly
- Ensure port 9000 is accessible between nodes
- Check for Docker network issues

### Replication Failures

**Symptoms:** `replication failed: connection refused`

**Diagnosis:**
```bash
# Check mesh node listener
docker exec space-node-1 netstat -tlnp | grep 9000

# Test direct TCP connection
docker exec space-node-1 nc -zv space-node-2 9000

# Check replication handler logs
docker logs space-node-2 2>&1 | grep "replication"
```

**Resolution:**
- Verify mesh node started successfully
- Check for MAC validation failures (key mismatch)
- Ensure sufficient disk space on target

### Gossip Convergence Slow

**Symptoms:** `avg_convergence_ms > 1000`

**Diagnosis:**
```bash
# Check gossip bandwidth
curl http://localhost:8081/api/gossip/stats | jq .bandwidth_usage

# Monitor message queue depth
docker logs space-node-1 2>&1 | grep "gossip queue"
```

**Resolution:**
- Reduce `SPACE_GOSSIP_FANOUT` if bandwidth constrained
- Increase `heartbeat_interval_ms` to reduce chattiness
- Verify network latency between nodes (`ping`)

---

## Advanced Topics

### Transformation in Transit

SPACE supports re-encryption and re-compression during migration:

```rust
// In scaling agent
let transform = mesh_state.requires_transformation(destination, policy);

if transform {
    // Decrypt with old key
    let plaintext = decrypt_segment(&ciphertext, old_key, &metadata)?;

    // Re-encrypt with new key
    let (new_ciphertext, new_meta) = encrypt_segment(
        &plaintext,
        new_key,
        new_version,
        new_tweak
    )?;

    // Send transformed segment
    mesh_node.send_replication_frame(&frame, destination).await?;
}
```

**Use cases:**
- Key rotation during migration
- Compression level changes
- Moving between encryption domains

### Sovereignty Constraints

PODMS enforces data sovereignty at three levels:

1. **Local:** Data never leaves the node
2. **Zone:** Data replicates within metro/geo zone only
3. **Global:** Data can replicate anywhere

```yaml
# Example: EU data sovereignty
policy:
  sovereignty: zone  # Restrict to EU zone

# Compiler ensures:
# - Replication targets are in same zone
# - Migrations respect zone boundaries
# - Federated views respect sovereignty
```

### Phase 4: Raft Integration

For strong consistency on critical metadata:

```rust
#[cfg(feature = "phase4")]
{
    // Use Raft for metadata shard consensus
    let cluster = RaftCluster::for_zone(&zone);
    cluster.store_shard(&shard_key, &metadata).await?;
}
```

This enables:
- Linearizable metadata reads
- Distributed locking
- Coordinated schema changes

---

## References

- [SPACE Architecture](./architecture.md)
- [PODMS Design](./podms.md)
- [Security Model](./security.md)
- [API Reference](./API.md)
- [Performance Tuning](./performance.md)

## Support

- GitHub Issues: https://github.com/saworbit/SPACE/issues
- Slack: https://space-project.slack.com
- Email: support@adaptive-storage.dev

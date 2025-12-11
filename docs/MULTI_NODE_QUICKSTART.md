# SPACE Multi-Node Quick Start

Get a 3-node SPACE mesh running in under 5 minutes.

## Prerequisites

- Docker & Docker Compose installed
- 8GB RAM available
- Ports 8081-8083, 9001-9003, 9090, 3000 available

## Quick Start

### 1. Start the Mesh

```bash
# From the SPACE repository root
docker-compose -f docker-compose.multi-node.yml up --build
```

This launches:
- **3 SPACE nodes** in a mesh configuration
- **Prometheus** for metrics collection
- **Grafana** for visualization

### 2. Access the Interfaces

Wait ~30 seconds for startup, then open:

- **Node 1 Web UI**: http://localhost:8081
- **Node 2 Web UI**: http://localhost:8082
- **Node 3 Web UI**: http://localhost:8083
- **Grafana Dashboard**: http://localhost:3000 (admin/space)
- **Prometheus**: http://localhost:9090

### 3. Verify Mesh Formation

Check that nodes have discovered each other:

```bash
# Check Node 1 peers
curl http://localhost:8081/api/peers | jq

# Expected output: 2 connected peers
```

### 4. Test Replication

Upload a file to Node 1:

```bash
# Create test file
echo "Hello from SPACE multi-node!" > test.txt

# Upload via S3 API
aws s3 --endpoint-url http://localhost:9001 \
  cp test.txt s3://test-bucket/test.txt
```

Watch the logs for replication:

```bash
docker logs -f space-node-1 | grep "replication"
```

### 5. Monitor the System

**Grafana Dashboards** (http://localhost:3000):
1. Login with admin/space
2. Navigate to "SPACE Multi-Node" dashboard
3. View:
   - Mesh topology
   - Replication throughput
   - Gossip message rates
   - Storage capacity per node

**Prometheus Metrics** (http://localhost:9090):
- Query: `space_gossip_messages_sent_total`
- Query: `space_replication_segments_sent_total`
- Query: `space_capsules_created_total`

## What's Happening Under the Hood

### Mesh Formation
1. Node 1 starts as **seed node** (SPACE_SEED_PEERS="")
2. Nodes 2 & 3 connect to seed (SPACE_SEED_PEERS="172.20.0.10:9000")
3. Gossip protocol exchanges peer lists
4. All nodes discover each other within ~5 seconds

### Autonomous Replication
1. Client writes capsule to Node 1
2. Node 1 commits to local NVRAM
3. **Gossip**: "NewCapsule" event broadcast
4. **Policy Compiler**: Evaluates metro-sync policy (zero-RPO)
5. **Scaling Agent**: Triggers replication to 2 targets
6. **Mesh Network**: Zero-copy segment streaming
7. Nodes 2 & 3 receive, validate MAC, dedup, persist

### Gossip Protocol
- **Fanout**: 8 peers (configurable)
- **Heartbeat**: Every 1000ms
- **TTL**: 10 hops max
- **Signing**: HMAC-SHA256
- **Bandwidth**: <1% overhead

## Configuration

Each node is configured via environment variables in `docker-compose.multi-node.yml`:

```yaml
environment:
  - SPACE_NODE_ID=node-1
  - SPACE_ZONE=us-west-metro
  - SPACE_LISTEN_ADDR=0.0.0.0:9000
  - SPACE_SEED_PEERS=172.20.0.10:9000  # Empty for seed node
  - SPACE_DEFAULT_POLICY=metro-sync
  - SPACE_GOSSIP_FANOUT=8
```

## Customization

### Change Replication Policy

Edit `docker-compose.multi-node.yml`:

```yaml
# Metro-sync (zero-RPO, synchronous)
- SPACE_DEFAULT_POLICY=metro-sync

# Async-batch (5min RPO, bandwidth-optimized)
- SPACE_DEFAULT_POLICY=async-batch

# No replication (local only)
- SPACE_DEFAULT_POLICY=no-replication
```

### Scale to More Nodes

Add more service definitions:

```yaml
space-node-4:
  build: .
  container_name: space-node-4
  environment:
    - SPACE_NODE_ID=node-4
    - SPACE_SEED_PEERS=172.20.0.10:9000
  ports:
    - "9004:9004"
  networks:
    space-mesh:
      ipv4_address: 172.20.0.13
```

### Adjust Gossip Parameters

```yaml
- SPACE_GOSSIP_FANOUT=16        # More peers (higher bandwidth)
- SPACE_HEARTBEAT_INTERVAL_MS=500  # Faster convergence
- SPACE_MESSAGE_TTL=20           # Longer propagation distance
```

## Troubleshooting

### Peers Not Connecting

**Check logs:**
```bash
docker logs space-node-1 | grep "gossip"
```

**Common fixes:**
- Ensure seed peer address is correct
- Check firewall rules
- Verify network connectivity: `docker exec space-node-1 ping space-node-2`

### Replication Not Working

**Verify mesh listener:**
```bash
docker exec space-node-1 netstat -tlnp | grep 9000
```

**Check replication logs:**
```bash
docker logs space-node-2 | grep "replication"
```

**Common fixes:**
- Ensure port 9000 is accessible
- Check NVRAM log has space
- Verify encryption keys match

### High Memory Usage

**Check container stats:**
```bash
docker stats
```

**Reduce resource usage:**
- Decrease gossip fanout
- Increase heartbeat interval
- Limit concurrent operations

## Next Steps

1. **Read the full guide**: [Multi-Node Deployment](./multi-node-deployment.md)
2. **Understand the architecture**: [Implementation Summary](./MULTI_NODE_IMPLEMENTATION.md)
3. **Integrate with your app**: Use the orchestrator APIs
4. **Deploy to production**: Follow security hardening guide

## Clean Up

Stop and remove all containers:

```bash
docker-compose -f docker-compose.multi-node.yml down -v
```

This removes:
- All containers
- Networks
- Volumes (persistent data)

## Getting Help

- **Documentation**: [docs/](.)
- **Issues**: https://github.com/saworbit/SPACE/issues

---

**Happy multi-noding! 🌐**

# Next-Gen Web Interface for SPACE Mesh Data System

## Overview

The SPACE web interface provides a comprehensive, real-time dashboard for managing and monitoring the mesh data system with integrated gossip protocol support. This implementation follows the **Next-Gen Web Interface Specification** with full support for epidemic-style state propagation, peer discovery, and event notification across distributed nodes.

For the modular monolith boundaries and the MVC split between Axum (controllers), shared state (model), and Leptos (views), see `docs/specs/SPACE_ARCHITECTURE_DESIGN_SPEC.md`.

## Architecture

The web interface consists of three main crates:

### 1. **mesh-core** - Core Types and Traits

Foundation layer providing:
- Type definitions for peers, nodes, and gossip messages
- Trait abstractions for storage backends and gossip handlers
- Error types and result wrappers
- Configuration structures

**Location**: `crates/mesh-core/`

### 2. **gossip-layer** - Gossip Protocol Implementation

Implements epidemic-style message propagation using libp2p:
- **Probabilistic propagation** with configurable fanout
- **Message signing and verification** (HMAC-SHA256)
- **TTL-based flood control**
- **Heartbeat-based liveness detection**
- **Topic-based pub/sub**

**Location**: `crates/gossip-layer/`

### 3. **web-interface** - HTTP Server and Frontend

Full-stack web application:
- **Backend**: Axum-based HTTP server with REST APIs and WebSocket support
- **Frontend**: Leptos reactive components (optional, feature-gated)
- **Real-time updates**: WebSocket integration with gossip layer
- **Metrics**: Prometheus-compatible metrics endpoint

**Location**: `crates/web-interface/`

## Key Features

### Real-Time Mesh Topology Visualization
- Dynamic peer discovery and status updates
- Gossip propagation path visualization
- Storage usage and health metrics

### Gossip Protocol Monitoring
- Convergence time metrics
- Message duplication rates
- Bandwidth usage tracking
- Active topics and peer counts

### File Operations
- Interactive file upload via web dashboard
- Chunked file uploads with gossip notifications
- Base64-encoded content transfer
- Blake3 hash verification
- File listing with metadata (size, hash, upload time)
- File download with binary content delivery
- In-memory storage with auto-refresh
- Automatic replication notifications

### Data Transformation
- ETL operations with gossip propagation
- Transformation status tracking
- Migration intent broadcasting

### Security
- Message authentication (HMAC-SHA256)
- Role-based access control (RBAC)
- Security alert broadcasting
- Zero-trust architecture ready

### WebSocket Real-Time Updates
- Subscribe to gossip topics
- Live peer updates
- Event streaming
- Heartbeat notifications

## Getting Started

### Prerequisites

- Rust 1.75+ (for async trait support)
- Cargo workspace setup
- Optional: wasm-pack for frontend builds

### Building

```bash
# Build all crates
cargo build --release

# Build with frontend support
cargo build --release --features frontend

# Build only the backend
cargo build --release -p web-interface
```

### Running the Server

```bash
# Run with default configuration
cargo run -p web-interface --bin web-server

# Run with custom configuration
BIND_ADDR=0.0.0.0:8080 \
GOSSIP_FANOUT=12 \
GOSSIP_HEARTBEAT_MS=500 \
GOSSIP_TTL=15 \
cargo run -p web-interface --bin web-server
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `BIND_ADDR` | Server bind address | `127.0.0.1:3000` |
| `GOSSIP_FANOUT` | Number of peers to gossip to | `8` |
| `GOSSIP_HEARTBEAT_MS` | Heartbeat interval in ms | `1000` |
| `GOSSIP_TTL` | Message time-to-live (max hops) | `10` |
| `GOSSIP_SIGNING_KEY` | Hex-encoded signing key (32 bytes) | Default dev key |

## API Reference

### REST Endpoints

#### GET /health
Health check endpoint.

**Response**: `200 OK` with "OK" text.

#### GET /api/peers
Get all known peers with gossip metrics.

**Response**:
```json
{
  "peers": [
    {
      "id": "peer-123",
      "addr": "127.0.0.1:8080",
      "role": "Viewer",
      "storage_usage": 104857600,
      "status": "online",
      "gossip_version": 1,
      "last_gossip_heartbeat": 1700000000
    }
  ],
  "gossip_metrics": {
    "convergence_time_ms": 0.5,
    "duplication_rate": 0.05,
    "bandwidth_usage": 1024
  },
  "total_count": 1
}
```

#### GET /api/peers/:peer_id
Get a specific peer by ID.

**Response**: `200 OK` with peer object, or `404 Not Found`.

#### POST /api/upload
Upload a file to the mesh.

**Request**:
```json
{
  "path": "/data/myfile.txt",
  "content": "SGVsbG8sIFdvcmxkIQ==" // base64 encoded
}
```

**Response**:
```json
{
  "success": true,
  "message": "File uploaded successfully: /data/myfile.txt",
  "hash": "abc123..."
}
```

#### GET /api/files
List all stored files with metadata.

**Response**:
```json
{
  "files": [
    {
      "path": "/data/myfile.txt",
      "size": 13,
      "hash": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
      "uploaded_at": 1700000000
    }
  ],
  "total": 1,
  "total_size": 13
}
```

#### GET /api/files/*path
Download a specific file by its path.

**Parameters**: `path` - File path (e.g., `/api/files/data/myfile.txt`)

**Response**: `200 OK` with file content as binary data, or `404 Not Found`.

#### GET /api/gossip/stats
Get gossip protocol statistics.

**Response**:
```json
{
  "stats": {
    "messages_sent": 1000,
    "messages_received": 950,
    "avg_convergence_ms": 0.8,
    "duplication_rate": 0.05,
    "active_topics": 5,
    "connected_peers": 10,
    "bandwidth_usage": 2048
  },
  "additional": {
    "peer_count": 10
  }
}
```

#### POST /api/gossip/broadcast
Broadcast a custom gossip message.

**Request**:
```json
{
  "topic": "custom-events",
  "payload": [1, 2, 3, 4]
}
```

**Response**:
```json
{
  "success": true,
  "message": "Message broadcasted successfully",
  "hash": null
}
```

#### GET /api/metrics
Get Prometheus-compatible metrics.

**Response**: Text format Prometheus metrics.

### WebSocket Endpoints

#### WS /ws/live
Real-time updates via WebSocket.

**Client Messages**:
```json
// Subscribe to a topic
{
  "type": "Subscribe",
  "topic": "updates"
}

// Unsubscribe from a topic
{
  "type": "Unsubscribe",
  "topic": "updates"
}

// Ping
{
  "type": "Ping"
}
```

**Server Messages**:
```json
// Gossip update
{
  "type": "GossipUpdate",
  "topic": "updates",
  "message": "Heartbeat { peer_id: \"node-1\", ... }"
}

// Subscription confirmation
{
  "type": "subscribed",
  "topic": "updates"
}

// Error
{
  "type": "Error",
  "message": "Failed to subscribe: ..."
}

// Pong
{
  "type": "Pong"
}
```

## Testing

### Unit Tests

```bash
# Test all crates
cargo test

# Test specific crate
cargo test -p mesh-core
cargo test -p gossip-layer
cargo test -p web-interface
```

### Integration Tests

```bash
# Run integration tests
cargo test --test integration_test

# Run with output
cargo test --test integration_test -- --nocapture
```

### Coverage

```bash
# Install tarpaulin
cargo install cargo-tarpaulin

# Run coverage
cargo tarpaulin --workspace --out Html
```

## Configuration

### Gossip Protocol Tuning

The gossip protocol can be tuned for different network conditions:

#### Fast Convergence (Small Networks, <100 nodes)
```rust
GossipConfig {
    fanout: 12,
    heartbeat_interval_ms: 500,
    message_ttl: 5,
    ..Default::default()
}
```

#### Balanced (Medium Networks, 100-1000 nodes)
```rust
GossipConfig {
    fanout: 8,
    heartbeat_interval_ms: 1000,
    message_ttl: 10,
    ..Default::default()
}
```

#### Low Bandwidth (Large Networks, 1000+ nodes)
```rust
GossipConfig {
    fanout: 6,
    heartbeat_interval_ms: 2000,
    message_ttl: 15,
    enable_compression: true,
    ..Default::default()
}
```

## Performance Considerations

### Gossip Overhead
- Typical overhead: <5% of total bandwidth
- Zero-copy message passing via `bytes` crate
- Batch message processing for efficiency

### Scalability
- Tested up to 1,000 simulated nodes
- O(log n) convergence time
- Topic sharding for >10,000 nodes

### Memory Usage
- LRU cache for peer views (configurable size)
- Message deduplication via hash-based tracking
- Automatic cleanup of stale connections

## Security

### Message Authentication
All gossip messages are signed using HMAC-SHA256:
- 32-byte signing key (configurable)
- Signature verification on receive
- Byzantine fault detection ready

### Encryption
- Optional message encryption (AES-GCM via `ring`)
- TLS for HTTP/WebSocket (recommended in production)
- Zero-trust architecture compatible

### RBAC
Node roles supported:
- **Admin**: Full control
- **Editor**: Read/write data
- **Viewer**: Read-only access
- **StorageNode**: Storage provider
- **Gateway**: External access point

## Troubleshooting

### Slow Convergence
- Increase `fanout` parameter
- Decrease `heartbeat_interval_ms`
- Check network latency between peers

### High Message Duplication
- Decrease `fanout` parameter
- Increase `message_ttl` to reduce retransmissions
- Enable message compression

### WebSocket Connection Issues
- Check CORS configuration
- Verify bind address is accessible
- Check firewall rules for WebSocket traffic

### Gossip Not Working
- Verify signing key matches across peers
- Check that peers can reach each other
- Review logs for authentication errors

## Future Enhancements

### Planned Features
- [ ] AI-tuned gossip parameters (using `linfa` ML library)
- [ ] Holographic data flow visualization
- [ ] Voice-command integration for AR
- [ ] Post-quantum cryptography support
- [ ] Federated search over gossiped indices
- [ ] Self-organizing storage rebalancing

### Experimental
- [ ] Browser-based peers via Wasm
- [ ] Zero-knowledge gossip for privacy
- [ ] Integration with TEE (Trusted Execution Environments)
- [ ] Quantum-resistant signatures

## Contributing

### Code Style
- Follow Rust standard formatting (`cargo fmt`)
- Run Clippy before submitting (`cargo clippy`)
- Add documentation for public APIs
- Include tests for new features

### Testing Requirements
- Unit tests for all new functions
- Integration tests for API endpoints
- Property-based tests for gossip convergence
- Minimum 85% code coverage

## License

Apache 2.0 (consistent with SPACE project)

## References

- [Gossip Protocol Overview](https://en.wikipedia.org/wiki/Gossip_protocol)
- [libp2p Gossipsub Specification](https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/README.md)
- [Axum Documentation](https://docs.rs/axum)
- [Leptos Framework](https://leptos.dev)
- [SPACE Project Main README](../README.md)

# SPACE Web Interface API Documentation

## Base URL

```
http://localhost:3000
```

Default port is 3000, configurable via `BIND_ADDR` environment variable.

## Authentication

Currently, the API is open for development purposes. Production deployments should:
- Enable TLS/HTTPS
- Implement JWT-based authentication
- Use the RBAC system for authorization

## REST API Endpoints

### Health Check

#### GET /health

Check if the server is running.

**Response:**
- Status: `200 OK`
- Body: `OK`

**Example:**
```bash
curl http://localhost:3000/health
```

---

### Peer Management

#### GET /api/peers

Get all known peers in the mesh with gossip metrics.

**Response:**
```json
{
  "peers": [
    {
      "id": "12D3KooWRq4...",
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
    "bandwidth_usage": 1024.0
  },
  "total_count": 1
}
```

**Fields:**
- `id`: Unique peer identifier (libp2p PeerId)
- `addr`: Network address (IP:port)
- `role`: Node role (Admin, Viewer, Editor, StorageNode, Gateway)
- `storage_usage`: Current storage in bytes
- `status`: Current status (online, offline, degraded)
- `gossip_version`: Protocol version for compatibility
- `last_gossip_heartbeat`: Unix timestamp of last heartbeat

**Example:**
```bash
curl http://localhost:3000/api/peers | jq
```

---

#### GET /api/peers/:peer_id

Get information about a specific peer.

**Parameters:**
- `peer_id` (path): The peer's unique identifier

**Response:**
- Status: `200 OK` with peer object
- Status: `404 Not Found` if peer doesn't exist

**Example:**
```bash
curl http://localhost:3000/api/peers/12D3KooWRq4... | jq
```

---

### File Operations

#### POST /api/upload

Upload a file to the mesh storage.

**Request Body:**
```json
{
  "path": "/data/myfile.txt",
  "content": "SGVsbG8sIFdvcmxkIQ=="
}
```

**Fields:**
- `path`: Destination path in the mesh
- `content`: Base64-encoded file content

**Response:**
```json
{
  "success": true,
  "message": "File uploaded successfully: /data/myfile.txt",
  "hash": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
}
```

**Fields:**
- `success`: Operation success status
- `message`: Human-readable message
- `hash`: Blake3 hash of the uploaded content

**Example:**
```bash
# Encode file content
CONTENT=$(base64 -w 0 myfile.txt)

# Upload
curl -X POST http://localhost:3000/api/upload \
  -H "Content-Type: application/json" \
  -d "{\"path\":\"/data/myfile.txt\",\"content\":\"$CONTENT\"}"
```

**Notes:**
- Files are stored in-memory on the web interface node
- Triggers a `FileUploaded` gossip message to notify peers
- Hash can be used for verification and deduplication
- Maximum file size limited by available memory

---

#### GET /api/files

List all stored files with metadata.

**Response:**
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

**Fields:**
- `files`: Array of file metadata objects
  - `path`: File path in the mesh
  - `size`: File size in bytes
  - `hash`: Blake3 hash of the file content
  - `uploaded_at`: Unix timestamp of upload
- `total`: Total number of stored files
- `total_size`: Total size of all files in bytes

**Response Headers:**
- `Content-Type: application/json`

**Example:**
```bash
curl http://localhost:3000/api/files | jq
```

**Notes:**
- Files are sorted by upload time (newest first)
- Returns empty array if no files stored

---

#### GET /api/files/*path

Download a specific file by its path.

**Parameters:**
- `path` (path): The file path (e.g., `/data/myfile.txt` becomes `/api/files/data/myfile.txt`)

**Response:**
- Status: `200 OK` with file content as binary data
- Status: `404 Not Found` if file doesn't exist

**Response Headers:**
- `Content-Type: application/octet-stream`

**Example:**
```bash
# Download file
curl http://localhost:3000/api/files/data/myfile.txt -o downloaded.txt

# Or view content
curl http://localhost:3000/api/files/data/myfile.txt
```

**Notes:**
- Returns raw file bytes
- Path must match exactly (case-sensitive)
- Browser downloads trigger automatic file save

---

### Gossip Protocol

#### GET /api/gossip/stats

Get statistics about the gossip protocol performance.

**Response:**
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

**Fields:**
- `messages_sent`: Total gossip messages sent
- `messages_received`: Total gossip messages received
- `avg_convergence_ms`: Average convergence time in milliseconds
- `duplication_rate`: Message duplication rate (0.0-1.0)
- `active_topics`: Number of active gossip topics
- `connected_peers`: Number of connected peers
- `bandwidth_usage`: Bandwidth usage in bytes/second

**Example:**
```bash
curl http://localhost:3000/api/gossip/stats | jq
```

---

#### POST /api/gossip/broadcast

Broadcast a custom message via gossip protocol.

**Request Body:**
```json
{
  "topic": "custom-events",
  "payload": [1, 2, 3, 4]
}
```

**Fields:**
- `topic`: Gossip topic to broadcast on
- `payload`: Binary payload as byte array

**Response:**
```json
{
  "success": true,
  "message": "Message broadcasted successfully",
  "hash": null
}
```

**Example:**
```bash
curl -X POST http://localhost:3000/api/gossip/broadcast \
  -H "Content-Type: application/json" \
  -d '{"topic":"events","payload":[72,101,108,108,111]}'
```

**Use Cases:**
- Custom event notifications
- Coordination messages
- Application-specific signaling

---

### Metrics

#### GET /api/metrics

Get Prometheus-compatible metrics.

**Response:**
- Content-Type: `text/plain; version=0.0.4`
- Body: Prometheus text format metrics

**Example:**
```bash
curl http://localhost:3000/api/metrics
```

**Sample Output:**
```
# HELP gossip_messages_sent Total gossip messages sent
# TYPE gossip_messages_sent counter
gossip_messages_sent 1000

# HELP gossip_convergence_time_seconds Gossip convergence time
# TYPE gossip_convergence_time_seconds histogram
gossip_convergence_time_seconds_bucket{le="0.001"} 10
gossip_convergence_time_seconds_bucket{le="0.01"} 50
...
```

**Integration:**
```yaml
# Prometheus scrape config
scrape_configs:
  - job_name: 'space-mesh'
    static_configs:
      - targets: ['localhost:3000']
    metrics_path: '/api/metrics'
```

---

## WebSocket API

### Real-Time Updates

#### WS /ws/live

WebSocket endpoint for real-time mesh updates.

**Connection:**
```javascript
const ws = new WebSocket('ws://localhost:3000/ws/live');
```

**Client → Server Messages:**

##### Subscribe to Topic
```json
{
  "type": "Subscribe",
  "topic": "updates"
}
```

Available topics:
- `updates` - General mesh updates
- `heartbeat` - Peer heartbeats
- `data_ops` - File operations
- `security` - Security alerts
- Custom topics

##### Unsubscribe from Topic
```json
{
  "type": "Unsubscribe",
  "topic": "updates"
}
```

##### Ping
```json
{
  "type": "Ping"
}
```

**Server → Client Messages:**

##### Subscription Confirmed
```json
{
  "type": "subscribed",
  "topic": "updates"
}
```

##### Unsubscription Confirmed
```json
{
  "type": "unsubscribed",
  "topic": "updates"
}
```

##### Gossip Update
```json
{
  "type": "GossipUpdate",
  "topic": "updates",
  "message": "Heartbeat { peer_id: \"node-1\", storage_usage: 1024, timestamp: 1700000000 }"
}
```

##### Error
```json
{
  "type": "Error",
  "message": "Failed to subscribe: topic not found"
}
```

##### Pong
```json
{
  "type": "Pong"
}
```

**Example Usage:**

```javascript
const ws = new WebSocket('ws://localhost:3000/ws/live');

ws.onopen = () => {
  console.log('Connected to SPACE mesh');

  // Subscribe to updates
  ws.send(JSON.stringify({
    type: 'Subscribe',
    topic: 'updates'
  }));
};

ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);

  switch (msg.type) {
    case 'subscribed':
      console.log('Subscribed to:', msg.topic);
      break;
    case 'GossipUpdate':
      console.log('Update on', msg.topic, ':', msg.message);
      break;
    case 'Error':
      console.error('Error:', msg.message);
      break;
  }
};

ws.onerror = (error) => {
  console.error('WebSocket error:', error);
};

ws.onclose = () => {
  console.log('Disconnected from SPACE mesh');
};
```

---

## Gossip Message Types

The gossip layer supports various message types:

### PeerUpdate
```json
{
  "type": "PeerUpdate",
  "peers": [...]
}
```

### DataMigration
```json
{
  "type": "DataMigration",
  "path": "/data/file.txt",
  "target_peer": "node-2",
  "size": 1024
}
```

### TransformationNotify
```json
{
  "type": "TransformationNotify",
  "path": "/data/file.txt",
  "op": "compress",
  "status": "success"
}
```

### SecurityAlert
```json
{
  "type": "SecurityAlert",
  "severity": "high",
  "threat": "Unauthorized access attempt",
  "source_peer": "node-1",
  "timestamp": 1700000000
}
```

### FileUploaded
```json
{
  "type": "FileUploaded",
  "path": "/data/file.txt",
  "size": 1024,
  "uploader": "web-interface",
  "hash": "abc123..."
}
```

### Heartbeat
```json
{
  "type": "Heartbeat",
  "peer_id": "node-1",
  "storage_usage": 1024,
  "timestamp": 1700000000
}
```

### Custom
```json
{
  "type": "Custom",
  "topic": "my-topic",
  "payload": [1, 2, 3]
}
```

---

## Error Handling

### HTTP Status Codes

- `200 OK` - Request successful
- `400 Bad Request` - Invalid request format or parameters
- `404 Not Found` - Resource not found
- `500 Internal Server Error` - Server error

### Error Response Format

```json
{
  "error": "Error message",
  "code": "ERROR_CODE",
  "details": "Additional context"
}
```

---

## Rate Limiting

Currently no rate limiting is implemented. For production use:
- Implement rate limiting middleware
- Use token bucket or sliding window algorithm
- Consider per-peer and per-topic limits

---

## CORS Configuration

The server is configured to allow all origins for development:
- `Access-Control-Allow-Origin: *`
- `Access-Control-Allow-Methods: *`
- `Access-Control-Allow-Headers: *`

For production, configure specific allowed origins.

---

## Configuration

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `BIND_ADDR` | Server bind address | `127.0.0.1:3000` |
| `GOSSIP_FANOUT` | Gossip fanout parameter | `8` |
| `GOSSIP_HEARTBEAT_MS` | Heartbeat interval (ms) | `1000` |
| `GOSSIP_TTL` | Message TTL (hops) | `10` |
| `GOSSIP_SIGNING_KEY` | Hex signing key (32 bytes) | Default dev key |

**Example:**
```bash
BIND_ADDR=0.0.0.0:8080 \
GOSSIP_FANOUT=12 \
GOSSIP_HEARTBEAT_MS=500 \
cargo run -p web-interface --bin web-server
```

---

## Best Practices

### Performance
- Use WebSockets for real-time updates instead of polling
- Subscribe only to needed topics
- Batch file uploads when possible
- Monitor gossip metrics to tune parameters

### Security
- Use HTTPS in production
- Implement authentication/authorization
- Validate all input data
- Rate limit API endpoints
- Use strong signing keys

### Reliability
- Implement retry logic with exponential backoff
- Handle WebSocket reconnections
- Monitor gossip convergence metrics
- Set appropriate TTL values

---

## Examples

### Python Client
```python
import requests
import json
import base64

# Upload a file
with open('data.txt', 'rb') as f:
    content = base64.b64encode(f.read()).decode()

response = requests.post('http://localhost:3000/api/upload', json={
    'path': '/data/myfile.txt',
    'content': content
})

print(response.json())
```

### Rust Client
```rust
use reqwest::Client;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();

    // Get peers
    let res = client
        .get("http://localhost:3000/api/peers")
        .send()
        .await?;

    let peers: serde_json::Value = res.json().await?;
    println!("{:#?}", peers);

    Ok(())
}
```

### JavaScript WebSocket
See the WebSocket section above for full example.

---

## Support

For issues and questions:
- GitHub Issues: https://github.com/saworbit/SPACE/issues
- Documentation: [docs/WEB_INTERFACE.md](WEB_INTERFACE.md)
- Main README: [../README.md](../README.md)

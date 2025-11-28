# 🚀 SPACE S3 Protocol View - Quick Start

## What This Proves

**Same capsule, different views.** This demo shows SPACE's core innovation:
- Write data via CLI → creates a capsule
- Read same data via S3 API → no copy, just a different protocol view

## Prerequisites

```bash
# Rust 1.78+
rustup update

# Build the project
cargo build --release
```

## Demo: One Capsule, Two Views

### Terminal 1: Start the S3 Server

```bash
./target/release/spacectl serve-s3 --port 8080
```

You should see:
```
🚀 SPACE S3 Protocol View listening on http://0.0.0.0:8080
📦 Ready to serve capsules via S3 API
```

### Terminal 2: Test the S3 API

#### 1. PUT an object (create capsule)

```bash
curl -X PUT http://localhost:8080/demo-bucket/hello.txt \
  -d "Hello from SPACE! This is stored as a capsule."
```

**Response:**
```
ETag: "550e8400-e29b-41d4-a716-446655440000"
```

#### 2. GET the object back

```bash
curl http://localhost:8080/demo-bucket/hello.txt
```

**Output:**
```
Hello from SPACE! This is stored as a capsule.
```

#### 3. HEAD to get metadata

```bash
curl -I http://localhost:8080/demo-bucket/hello.txt
```

**Output:**
```
HTTP/1.1 200 OK
content-length: 47
content-type: text/plain
etag: "550e8400-e29b-41d4-a716-446655440000"
```

#### 4. LIST objects in bucket

```bash
curl http://localhost:8080/demo-bucket
```

**Output:**
```json
{
  "name": "demo-bucket",
  "contents": [
    {
      "key": "demo-bucket/hello.txt",
      "size": 47,
      "content_type": "text/plain",
      "etag": "\"550e8400-e29b-41d4-a716-446655440000\""
    }
  ]
}
```

#### 5. Upload a binary file

```bash
dd if=/dev/urandom of=test.bin bs=1M count=5
curl -X PUT http://localhost:8080/demo-bucket/data.bin \
  --data-binary @test.bin

curl http://localhost:8080/demo-bucket/data.bin > downloaded.bin
diff test.bin downloaded.bin  # Should be identical
```

## Test with AWS CLI (Optional)

While SPACE S3 isn't feature-complete, you can test basic operations:

```bash
# Configure fake credentials (not used yet)
export AWS_ACCESS_KEY_ID=test
export AWS_SECRET_ACCESS_KEY=test

# Use with custom endpoint
aws s3 --endpoint-url http://localhost:8080 \
  cp localfile.txt s3://demo-bucket/uploaded.txt

aws s3 --endpoint-url http://localhost:8080 \
  ls s3://demo-bucket/
```

## Architecture Validation

This demo proves:

✅ **Universal Namespace** - Each S3 object maps to exactly one CapsuleId
✅ **Protocol Views** - Same data accessible via HTTP/REST (S3) and CLI
✅ **No Data Duplication** - PUT creates one capsule, GET reads that same capsule
✅ **Metadata Mapping** - S3 keys are resolved to capsules at runtime

## What's Next?

Now that protocol views work, you can:

1. **Add Authentication** - SPIFFE/mTLS integration
2. **Add NFS View** - Mount capsules as a filesystem
3. **Add Block View** - Expose capsules via NVMe-oF
4. **Add Encryption** - Per-segment XTS-AES-256
5. **Add Replication** - Metro-sync between nodes

## Troubleshooting

**Server won't start:**
```bash
# Check if port is in use
lsof -i :8080

# Try a different port
./target/release/spacectl serve-s3 --port 9000
```

**Can't write objects:**
```bash
# Check NVRAM log is writable
ls -la space.nvram*

# Clean slate (deletes all data!)
rm space.nvram* space.metadata
```

## Performance Notes

Current MVP is **NOT** production-ready:
- No caching layer
- No connection pooling
- No multipart upload support
- Limited error handling

But it **does prove the concept** - one capsule, infinite protocol views!

---

**🎯 Mission Accomplished:** You've proven SPACE's universal namespace works across protocols.

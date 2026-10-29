# S3 Quick Start

This walkthrough runs the S3-compatible view on one node, stores a few objects, and then reads one of them back through `spacectl` by its capsule ID. Everything below was run against the post-reset `main` branch.

The S3 view is alpha. See [Current limitations](#current-limitations) before pointing a real client at it.

## Prerequisites

- Rust 1.89 or newer. The committed `Cargo.lock` needs it ([#100](https://github.com/saworbit/SPACE/issues/100)).
- `curl`

```bash
cargo build --release -p spacectl
```

## 1. Start the server

```bash
./target/release/spacectl serve-s3 --port 8080
```

`--port` (or `-p`) defaults to 8080. The server stores its data in the current directory:

| Path | Contents |
|------|----------|
| `space.db/` | sled database: capsule records and the content-hash index |
| `space.nvram` | append-only segment log |
| `space.nvram.segments` | JSON map of segment offsets, lengths, compression and encryption metadata |

Set `SPACE_STORAGE_ROOT` to store segments as files under a directory instead of the NVRAM log. The registry stays in `./space.db`.

```bash
SPACE_STORAGE_ROOT=./space.storage ./target/release/spacectl serve-s3 --port 8080
```

Startup prints `Starting SPACE S3 Protocol View...`, then log lines including:

```
INFO protocol_s3::server: 🚀 SPACE S3 Protocol View listening on http://127.0.0.1:8080
```

The server has no authentication, so it listens on loopback only. `--bind 0.0.0.0` exposes it on every interface; do that only on a network you trust.

## 2. PUT an object

```bash
curl -i -X PUT http://localhost:8080/demo-bucket/hello.txt \
  -d "Hello from SPACE! This is stored as a capsule."
```

The response is `200 OK`, and the ETag is the new capsule's ID:

```
HTTP/1.1 200 OK
etag: "f1eec31a-8966-4ca1-9538-187d6529383b"
```

## 3. GET and HEAD

```bash
curl http://localhost:8080/demo-bucket/hello.txt
```

```
Hello from SPACE! This is stored as a capsule.
```

```bash
curl -I http://localhost:8080/demo-bucket/hello.txt
```

```
HTTP/1.1 200 OK
content-length: 46
content-type: text/plain
etag: "f1eec31a-8966-4ca1-9538-187d6529383b"
last-modified: Wed, 28 Oct 2026 19:17:00 GMT
```

The content type is guessed from the key's extension.

## 4. List a bucket

```bash
curl http://localhost:8080/demo-bucket
```

```json
{"name":"demo-bucket","prefix":null,"contents":[{"key":"demo-bucket/hello.txt","size":46,"last_modified":1793215020,"content_type":"text/plain","etag":"\"f1eec31a-8966-4ca1-9538-187d6529383b\""}]}
```

The listing is JSON, not S3's `ListBucketResult` XML, and each `key` includes the bucket name ([#89](https://github.com/saworbit/SPACE/issues/89)).

## 5. Round-trip a binary file

```bash
dd if=/dev/urandom of=test.bin bs=1M count=5
curl -X PUT http://localhost:8080/demo-bucket/data.bin --data-binary @test.bin
curl -o downloaded.bin http://localhost:8080/demo-bucket/data.bin
cmp test.bin downloaded.bin && echo identical
```

Use `--data-binary`. Plain `-d` strips newlines.

## 6. Read the same capsule through spacectl

The S3 view and `spacectl` share one capsule store. Stop the server first: sled allows only one process to open `space.db`, so `spacectl` can't run alongside `serve-s3` ([#92](https://github.com/saworbit/SPACE/issues/92)). Then read the capsule by the ETag from step 2:

```bash
./target/release/spacectl list
./target/release/spacectl read f1eec31a-8966-4ca1-9538-187d6529383b
```

The bucket/key → capsule map lives only in the server's memory. After a restart, every capsule is still in `spacectl list`, but S3 GETs return 404 until the objects are written again ([#85](https://github.com/saworbit/SPACE/issues/85)).

## Using an S3 SDK

With path-style addressing, boto3's `put_object`, `get_object`, `head_object` and `delete_object` work. Credentials are ignored.

```python
import boto3
from botocore.config import Config

s3 = boto3.client("s3", endpoint_url="http://localhost:8080",
                  aws_access_key_id="x", aws_secret_access_key="x", region_name="us-east-1",
                  config=Config(s3={"addressing_style": "path"}))
s3.put_object(Bucket="demo-bucket", Key="k.txt", Body=b"hi")
print(s3.get_object(Bucket="demo-bucket", Key="k.txt")["Body"].read())
```

`list_objects_v2` fails, and so does `upload_file` above the 8 MiB multipart threshold ([#89](https://github.com/saworbit/SPACE/issues/89)). The AWS CLI hasn't been tested.

## Current limitations

- Keys can't contain `/` ([#88](https://github.com/saworbit/SPACE/issues/88)).
- No multipart upload, and listing isn't S3 XML ([#89](https://github.com/saworbit/SPACE/issues/89)).
- Whole objects are buffered in memory on PUT and GET, and `Range` is ignored ([#99](https://github.com/saworbit/SPACE/issues/99)).
- DELETE and overwrites don't free storage ([#86](https://github.com/saworbit/SPACE/issues/86)).
- Objects are encrypted only when `SPACE_MASTER_KEY` is set. There is no per-request choice.
- Error responses are plain text, not S3 XML error codes.

## Troubleshooting

**Port already in use:** pick another port with `--port 9000`. To see what holds the port, run `lsof -i :8080` on Linux/macOS or `netstat -ano | findstr :8080` on Windows.

**`spacectl` fails with "could not acquire lock" on `space.db`** (`list` panics; the other commands print an error): a `serve-s3` process is still running in that directory.

**Start from an empty store (deletes all data):**

```bash
rm -rf space.db space.nvram space.nvram.segments
```

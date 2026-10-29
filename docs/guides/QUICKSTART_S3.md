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

```xml
<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>demo-bucket</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Marker></Marker><Contents><Key>hello.txt</Key><LastModified>2026-10-28T19:17:00.000Z</LastModified><ETag>&quot;f1eec31a-8966-4ca1-9538-187d6529383b&quot;</ETag><Size>46</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>
```

This is S3's `ListBucketResult`. Add `?list-type=2` for ListObjectsV2. `prefix`, `delimiter`, `max-keys` and paging work. Keys may contain `/`. A PUT creates its bucket if needed, and `curl -X PUT http://localhost:8080/new-bucket` creates an empty one.

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

Buckets and keys are stored in `space.db` too, so objects are still there after the server restarts. `spacectl delete <id>` removes a capsule. An S3 key that pointed at it then returns `NoSuchKey`.

## Using an S3 SDK

With path-style addressing, boto3's `create_bucket`, `head_bucket`, `put_object`, `get_object`, `head_object`, `delete_object`, `list_objects` and `list_objects_v2` work, paginators included. Credentials are ignored.

```python
import boto3
from botocore.config import Config

s3 = boto3.client("s3", endpoint_url="http://localhost:8080",
                  aws_access_key_id="x", aws_secret_access_key="x", region_name="us-east-1",
                  config=Config(s3={"addressing_style": "path"}))
s3.put_object(Bucket="demo-bucket", Key="k.txt", Body=b"hi")
print(s3.get_object(Bucket="demo-bucket", Key="k.txt")["Body"].read())
```

`upload_file` fails above the 8 MiB multipart threshold, because there is no multipart upload. The AWS CLI hasn't been tested.

## Current limitations

- No multipart upload.
- Whole objects are buffered in memory on PUT and GET, and `Range` is ignored ([#99](https://github.com/saworbit/SPACE/issues/99)).
- DELETE and overwrites free the old capsule, but its bytes stay in `space.nvram` until compaction, and nothing runs compaction yet ([#97](https://github.com/saworbit/SPACE/issues/97)).
- Objects are encrypted only when `SPACE_MASTER_KEY` is set. There is no per-request choice.

## Troubleshooting

**Port already in use:** pick another port with `--port 9000`. To see what holds the port, run `lsof -i :8080` on Linux/macOS or `netstat -ano | findstr :8080` on Windows.

**`spacectl` fails with "could not acquire lock" on `space.db`** (`list` panics; the other commands print an error): a `serve-s3` process is still running in that directory.

**Start from an empty store (deletes all data):**

```bash
rm -rf space.db space.nvram space.nvram.segments
```

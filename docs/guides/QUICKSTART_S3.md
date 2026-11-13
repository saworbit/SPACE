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

`--port` (or `-p`) defaults to 8080. The server stores its data in the current directory, or the one `--store <dir>` (or `SPACE_STORE_DIR`) names:

| Path | Contents |
|------|----------|
| `space.journal` | capsule journal: a record for each capsule write and delete and each bucket and key change; the durable record of which capsules, buckets and keys exist |
| `space.db/` | sled database: an index of the journal (capsule records, content-hash index, buckets and keys), rebuilt from the journal if it's lost |
| `space.nvram` | append-only segment log; each segment's compression and encryption metadata is stored with its bytes ([on-disk format](../on-disk-format.md)) |

A store created before on-disk format 1 has a `space.nvram.segments` file as well. This version refuses to open it.

Set `SPACE_STORAGE_ROOT` to store segments as files under a directory instead of the NVRAM log: one checksummed file per segment, plus a `store` file that pairs the directory with `./space.db`. A directory written by builds before on-disk format 1 is refused. Set it for every `spacectl` command run against the store, not just `serve-s3`: the journal is paired with one store, so a command run without it opens `space.nvram` and is refused.

```bash
SPACE_STORAGE_ROOT=./space.storage ./target/release/spacectl serve-s3 --port 8080
```

Once it's listening, it logs one line saying where, which store it serves, whether encryption is on, and when it reclaims and scrubs:

```
INFO spacectl: listening addr=127.0.0.1:8080,[::1]:8080 store=/home/me/space segments=/home/me/space/space.nvram encryption=off reclaim=300s scrub=light:86400s,deep:604800s
```

The server has no authentication, so it listens on loopback only: on `127.0.0.1` and on `::1`, so clients that resolve `localhost` to `::1` first connect at once. `--bind <address>` listens on that address alone. `--bind 0.0.0.0` exposes it on every interface, and logs a warning; do that only on a network you trust.

While it runs, the server reclaims the space of deleted objects and interrupted uploads: in `space.nvram` by compacting it, under `SPACE_STORAGE_ROOT` by deleting the segment files no object references. It also compacts `space.journal`, dropping the records of deleted and overwritten objects. See [Current limitations](#current-limitations) for when.

It also scrubs the store, reading at most 64 MiB/s (`--scrub-rate`): each segment gets a deep scrub weekly (`--deep-scrub-interval`) and a light scrub daily (`--scrub-interval`, `0` to turn scrubbing off). When each segment was last checked is kept in `space.db`, so a restart only scrubs segments that are new or due. Damage is logged at `WARN`. With the server stopped, `spacectl scrub --deep` lists any damaged segments and the capsules that hold them.

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

Buckets and keys are recorded in `space.journal` too, so objects are still there after the server restarts. A PUT commits the object, its key and the removal of any object it replaces together, so a crash leaves either the old object or the new one under the key. `spacectl delete <id>` removes a capsule. An S3 key that pointed at it then returns `NoSuchKey`.

## Using an S3 SDK

With path-style addressing, these boto3 calls work: `create_bucket`, `head_bucket`, `get_bucket_location`, `put_object`, `get_object`, `head_object`, `delete_object`, `list_objects` and `list_objects_v2`, paginators included. Ranged GETs work, so `download_file` does too. Credentials are ignored, so presigned URLs work too. Anything else raises a `NotImplemented` error and leaves the store unchanged; see [Current limitations](#current-limitations).

```python
import boto3
from botocore.config import Config

s3 = boto3.client("s3", endpoint_url="http://localhost:8080",
                  aws_access_key_id="x", aws_secret_access_key="x", region_name="us-east-1",
                  config=Config(s3={"addressing_style": "path"}))
s3.put_object(Bucket="demo-bucket", Key="k.txt", Body=b"hi")
print(s3.get_object(Bucket="demo-bucket", Key="k.txt")["Body"].read())
```

`upload_file` fails with `NotImplemented` above the 8 MiB multipart threshold, because there is no multipart upload. The AWS CLI hasn't been tested.

## Current limitations

- These S3 operations aren't implemented, and each gets a `501 NotImplemented` error:
  - multipart upload;
  - CopyObject;
  - object tagging and ACLs, and bucket configuration such as `?versioning`;
  - ListBuckets, DeleteBucket and DeleteObjects.

  A PUT with an `aws-chunked` body is refused the same way, so send the body unencoded. Before [#156](https://github.com/saworbit/SPACE/issues/156), some of these ran as a plain PUT or DELETE and overwrote or deleted the object.
- DELETE and overwrites free the old capsule at once, but its bytes stay in `space.nvram` until compaction. The server runs a reclaim pass at startup and every 5 minutes (`--reclaim-interval <seconds>`, `0` to turn it off), and compacts once a quarter of the log, and at least 4 MiB, can be reclaimed. The same pass compacts `space.journal`, which records every PUT and DELETE, once a quarter of it, and at least 1 MiB, is dead. With the server stopped, `spacectl compact` compacts both right away.
- Objects are encrypted only when `SPACE_MASTER_KEY` is set. There is no per-request choice.
- Keys can't hold control characters other than tab, newline and carriage return, since a listing's XML can't carry them ([#172](https://github.com/saworbit/SPACE/issues/172)).

## Troubleshooting

**Port already in use:** pick another port with `--port 9000`. To see what holds the port, run `lsof -i :8080` on Linux/macOS or `netstat -ano | findstr :8080` on Windows.

**`spacectl` fails with `space.db is locked by another process`**: a `serve-s3` process, or another `spacectl` command, is still using that store. Only one can use a store at a time.

**`no SPACE store in .`**: `spacectl` only creates a store with `create` or `serve-s3`. Run the command from the store's directory, or point at it with `--store <dir>`.

**Start from an empty store (deletes all data):**

```bash
rm -rf space.db space.journal space.nvram space.nvram.segments space.storage
```

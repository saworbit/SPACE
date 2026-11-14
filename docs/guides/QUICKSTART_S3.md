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

The server answers only requests addressed to `localhost` or to an IP address, and refuses requests from web pages on other origins, so a web page can't reach it through your browser ([#200](https://github.com/saworbit/SPACE/issues/200)). To reach it by another name, such as `space.lan`, start it with `--allow-host space.lan` (repeat it for more names).

While it runs, the server reclaims the space of deleted objects and interrupted uploads: in `space.nvram` by compacting it, under `SPACE_STORAGE_ROOT` by deleting the segment files no object references. It also compacts `space.journal`, dropping the records of deleted and overwritten objects. See [Current limitations](#current-limitations) for when.

It also scrubs the store, reading at most 64 MiB/s (`--scrub-rate`): each segment gets a deep scrub weekly (`--deep-scrub-interval`) and a light scrub daily (`--scrub-interval`, `0` to turn scrubbing off). When each segment was last checked is kept in `space.db`, so a restart only scrubs segments that are new or due. Damage is logged at `WARN`. With the server stopped, `spacectl scrub --deep` lists any damaged segments and the capsules that hold them.

## 2. PUT an object

Create a bucket, then PUT an object into it:

```bash
curl -X PUT http://localhost:8080/demo-bucket
curl -i -X PUT http://localhost:8080/demo-bucket/hello.txt \
  -H "Content-Type: text/plain" \
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

GET and HEAD return the `Content-Type`, `Content-Disposition`, `Content-Encoding`, `Content-Language`, `Cache-Control`, `Expires` and `x-amz-meta-*` headers the PUT sent. Without a `Content-Type`, the type is guessed from the key's extension. `curl -d` sends `application/x-www-form-urlencoded` unless told otherwise, so step 2 sets it. As in S3, the `x-amz-meta-*` names and values can add up to at most 2 KB.

## 4. List a bucket

```bash
curl http://localhost:8080/demo-bucket
```

```xml
<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>demo-bucket</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Marker></Marker><Contents><Key>hello.txt</Key><LastModified>2026-10-28T19:17:00.000Z</LastModified><ETag>&quot;f1eec31a-8966-4ca1-9538-187d6529383b&quot;</ETag><Size>46</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>
```

This is S3's `ListBucketResult`. Add `?list-type=2` for ListObjectsV2. `prefix`, `delimiter`, `max-keys` and paging work. Keys may contain `/`. As in S3, a PUT into a bucket that doesn't exist fails with `NoSuchBucket`, and a new bucket's name is 3 to 63 lowercase letters, digits, dots and hyphens.

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

Buckets and keys are recorded in `space.journal` too, so objects are still there after the server restarts. A PUT commits the object, its key and the removal of any object it replaces together, so a crash leaves either the old object or the new one under the key. `spacectl delete <id>` removes a capsule, and any S3 key that points at it.

## Using an S3 SDK

With path-style addressing, these boto3 calls work: `list_buckets`, `create_bucket`, `head_bucket`, `delete_bucket`, `get_bucket_location`, `put_object`, `get_object`, `head_object`, `delete_object`, `delete_objects`, `list_objects` and `list_objects_v2`, paginators included. Ranged GETs work, so `download_file` does too. Credentials are ignored, so presigned URLs work too. Anything else raises a `NotImplemented` error and leaves the store unchanged; see [Current limitations](#current-limitations).

```python
import boto3
from botocore.config import Config

s3 = boto3.client("s3", endpoint_url="http://localhost:8080",
                  aws_access_key_id="x", aws_secret_access_key="x", region_name="us-east-1",
                  config=Config(s3={"addressing_style": "path"}))
s3.create_bucket(Bucket="demo-bucket")
s3.put_object(Bucket="demo-bucket", Key="k.txt", Body=b"hi")
print(s3.get_object(Bucket="demo-bucket", Key="k.txt")["Body"].read())
```

`upload_file` fails with `NotImplemented` above the 8 MiB multipart threshold, because there is no multipart upload ([#201](https://github.com/saworbit/SPACE/issues/201)). The AWS CLI hasn't been tested.

A PUT body is checked against the digests its headers give, as S3 does: `Content-MD5`, `x-amz-checksum-crc32`, `-crc32c`, `-crc64nvme`, `-sha1` and `-sha256`, and a hex `x-amz-content-sha256`. A body that doesn't match gets `400 BadDigest` (`XAmzContentSHA256Mismatch` for the last) and stores nothing ([#196](https://github.com/saworbit/SPACE/issues/196)). The SDKs send a CRC32 or CRC64NVME by default, so their uploads are checked.

A PUT body can also come in `aws-chunked` framing, as SDKs send streamed uploads: `STREAMING-AWS4-HMAC-SHA256-PAYLOAD`, `STREAMING-UNSIGNED-PAYLOAD-TRAILER` or `STREAMING-AWS4-HMAC-SHA256-PAYLOAD-TRAILER`. It's decoded as it arrives, and the object is the decoded bytes ([#178](https://github.com/saworbit/SPACE/issues/178)). Chunk signatures aren't checked, since credentials aren't. The decoded length must match `x-amz-decoded-content-length`, and a checksum the trailer carries, named in `x-amz-trailer`, is checked like one in a header. A body whose framing is wrong or cut short gets `400 InvalidRequest` and stores nothing.

## Current limitations

- These S3 operations aren't implemented, and each gets a `501 NotImplemented` error:
  - multipart upload ([#201](https://github.com/saworbit/SPACE/issues/201));
  - CopyObject;
  - object tagging and ACLs, and bucket configuration such as `?versioning`;
  - object versions: DeleteObjects reports a key given with a version id other than `null` as an error, and leaves it;
  - PUT headers asking for something the server can't do: Object Lock, server-side encryption options (objects are encrypted when the server has `SPACE_MASTER_KEY`), `x-amz-tagging`, ACLs other than `x-amz-acl: private`, storage classes other than `STANDARD`, website redirects, and checksum algorithms other than those above.

  Before [#156](https://github.com/saworbit/SPACE/issues/156), some of these ran as a plain PUT or DELETE and overwrote or deleted the object.
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

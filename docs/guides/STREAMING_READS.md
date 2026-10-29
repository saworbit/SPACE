# Streaming Reads and Paginated Listings

The pipeline can read a capsule one segment at a time, so a reader holds roughly one segment (4 MiB by default) in memory instead of the whole object. S3 GET and `spacectl read` both read this way, and S3 PUT and `spacectl create` write this way.

## Streaming a capsule

- `Pipeline::read_capsule_stream(id).await -> anyhow::Result<DataStream>`
- `RegistryPipelineHandle::read_capsule_stream(id).await`, the same call through the registry-backed handle
- `DataStream = Pin<Box<dyn Stream<Item = anyhow::Result<Bytes>> + Send>>`, defined in `common::traits`

The stream looks up the capsule once, then for each segment in turn reads its bytes, verifies and decrypts it if it is encrypted, decompresses it if it is compressed, and yields the plaintext as one `Bytes` chunk. A 9 MiB capsule arrives as three chunks. `read_capsule` is the same loop collected into a `Vec<u8>`.

Serving a stream with axum:

```rust
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use capsule_registry::modular_pipeline::RegistryPipelineHandle;
use common::CapsuleId;
use tokio::sync::Mutex;
use uuid::Uuid;

pub async fn download(
    Path(id): Path<Uuid>,
    State(pipeline): State<Arc<Mutex<RegistryPipelineHandle>>>,
) -> Response {
    let stream = pipeline
        .lock()
        .await
        .read_capsule_stream(CapsuleId::from_uuid(id))
        .await;
    match stream {
        Ok(stream) => Body::from_stream(stream).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}
```

The stream doesn't borrow the handle, so the lock is released before the body is sent. A delete can then run while the body is still going out. The S3 view holds off deleting a capsule until its last GET is done.

## Range reads

`read_range(id, offset, len)` returns the bytes in `[offset, offset + len)`, clamped to the capsule size. It uses each segment's `plain_len` to skip segments that end before `offset`. A segment that overlaps the range is still read, decrypted and decompressed in full, so a 4 KiB range costs at most two segment reads. `read_range_stream(id, offset, len)` does the same one segment at a time, and S3 GET uses it for `Range` requests.

## Streaming writes

`begin_write`, then `write_chunk` once per segment, then `finish_write` (or `abort_write`) stores a capsule one segment at a time. Every chunk but the last is exactly one segment long, so the capsule gets the same segments, and the same dedup, as `write_capsule` would give it. `modular_pipeline::write_from_reader` drives this from any `AsyncRead`. It locks the pipeline only while a segment is stored, and aborts the write if its future is dropped. S3 PUT and `spacectl create` use it.

## Paginated listings

- `CapsuleRegistry::list_capsules(limit, cursor) -> Result<Vec<CapsuleId>>` returns up to `limit` IDs after `cursor`, in ID order. Pass `None` for the first page, then the last ID of each page.
- `spacectl list` pages through the registry 256 IDs at a time.

## Tests

```bash
cargo test -p capsule-registry --test streaming_test
cargo test -p capsule-registry --test range_read_test
```

## Not implemented: read-time transforms

`Policy.transform` deserializes a list of WASM transform definitions, but there is no WASM runtime on this branch, so they are never applied.

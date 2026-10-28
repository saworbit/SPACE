# Streaming Reads and Paginated Listings

The pipeline can read a capsule one segment at a time, so a reader holds roughly one segment (4 MiB by default) in memory instead of the whole object. That holds for library callers today. Neither user-facing path uses the streaming read yet: S3 GET and `spacectl read` both buffer the whole capsule ([#99](https://github.com/saworbit/SPACE/issues/99)).

## Streaming a capsule

- `Pipeline::read_capsule_stream(id).await -> anyhow::Result<DataStream>`
- `RegistryPipelineHandle::read_capsule_stream(id).await`, the same call through the registry-backed handle
- `DataStream = Pin<Box<dyn Stream<Item = anyhow::Result<Bytes>> + Send>>`, defined in `common::traits`

The stream looks up the capsule once, then for each segment in turn reads its bytes, verifies and decrypts it if it is encrypted, decompresses it if it is compressed, and yields the plaintext as one `Bytes` chunk. A 9 MiB capsule arrives as three chunks. `read_capsule` is the same loop collected into a `Vec<u8>`.

Serving a stream with axum 0.7:

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

The stream doesn't borrow the handle, so the lock is released before the body is sent.

## Range reads

`read_range(id, offset, len)` returns the bytes in `[offset, offset + len)`, clamped to the capsule size. It uses each segment's `plain_len` to skip segments that end before `offset`. A segment that overlaps the range is still read, decrypted and decompressed in full, so a 4 KiB range costs at most two segment reads. S3 doesn't call it yet ([#99](https://github.com/saworbit/SPACE/issues/99)).

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

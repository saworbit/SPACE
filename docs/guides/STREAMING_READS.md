# Streaming Reads & Paginated Listings

Phase 1 replaces monolithic capsule loads with cursor-driven streams so memory stays flat, even for multi‑terabyte objects.

## Data Plane: Stream Capsule Reads
- API surface: `Pipeline::read_capsule_stream(id) -> DataStream` where `DataStream = Pin<Box<dyn Stream<Item = anyhow::Result<Bytes>> + Send>>`.
- Behavior: segments are fetched, decrypted, and decompressed lazily; `Bytes` ensures zero-copy slices downstream.
- Axum example (TTFB-first response):
```rust
use axum::body::StreamBody;
use capsule_registry::modular_pipeline::RegistryPipelineHandle;
use common::CapsuleId;
use std::sync::Arc;

pub async fn download(
    axum::extract::Path(id): axum::extract::Path<CapsuleId>,
    axum::extract::State(pipeline): axum::extract::State<Arc<RegistryPipelineHandle>>,
) -> impl axum::response::IntoResponse {
    match pipeline.read_capsule_stream(id).await {
        Ok(stream) => StreamBody::new(stream),
        Err(e) => (axum::http::StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}
```

## Control Plane: Cursor Pagination
- Trait: `MetadataStore::list_capsules(limit, start_after)` and `CapsuleRegistry::list_capsules(limit, cursor)` return a page of IDs.
- Cursor strategy: pass `None` for the first page; feed the last ID of the previous page into `cursor` to continue.
- `spacectl list` already paginates internally (default page size 256); use it for large registries without blowing RAM.

## Testing
- Feature-gated regression: `crates/capsule-registry/tests/streaming_test.rs` validates streaming consistency over multiple segments under `--features modular_pipeline`.

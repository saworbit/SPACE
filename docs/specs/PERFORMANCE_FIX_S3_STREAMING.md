# Performance Fix: S3 Protocol Streaming I/O

**Severity:** High  
**Target:** `crates/protocol-s3`  
**Status:** Proposed

## Problem Analysis
- The Axum handlers eagerly extract `Bytes`, forcing a full request/response buffer for every object.
- `put_object` double-allocates (`Bytes` -> `Vec<u8>`), creating O(N) memory pressure per inflight upload (5 GB upload → >5 GB RAM).
- `get_object` assembles the entire payload before replying, blocking the first byte to the client and preventing TCP backpressure from propagating.

## Proposed Solution: Streaming Interfaces
- Axum handlers accept `Body` and keep it as a stream instead of buffering.
- `S3View::put_object` consumes `impl Stream<Item = Result<Bytes, _>>` and bridges to the existing `[u8]` pipeline internally (temporary buffer).
- `S3View::get_object` returns a `Stream` over the stored bytes, enabling immediate response streaming.
- Bridge code uses `StreamReader`/`ReaderStream` to adapt between `Stream` and `AsyncRead`.

### Rationale
- Decouples HTTP streaming from the current buffered storage pipeline while preserving API compatibility.
- Isolates the temporary buffering to the protocol boundary, preparing for a future zero-copy pipeline.
- Enables backpressure-aware uploads/downloads today, even before the storage layer is upgraded.

## Data Flow (New)
- **PUT**: Client → Axum `Body` (stream) → `S3View::put_object` → buffer bridge → `WritePipeline`.
- **GET**: `WritePipeline` → buffer bridge → `S3View::get_object` → Axum `Body::from_stream` → Client.

## Verification
1. Integration test streams a multi-megabyte upload via Axum and asserts a 200 OK response.
2. Existing S3 view tests collect the streamed response and validate content integrity and metadata.

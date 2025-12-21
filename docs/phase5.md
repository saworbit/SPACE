# Phase 5: The Brain (Compute-over-Data / WASM Transforms)

Phase 5 embeds a sandboxed WASM runtime into the storage pipeline so capsules can be
transformed *in-flight* during reads (lazy) or during ingestion (eager).

## Goals

- **Compute-to-data:** move transformation logic (WASM) to the storage node.
- **Streaming-first:** process data chunk-by-chunk; no full-capsule buffering required.
- **Sandboxed safety:** traps/timeouts prevent buggy modules from crashing the node.

## Policy Schema

`Policy.transform` is an ordered chain. Output of transform `i` feeds transform `i+1`.

```yaml
transform:
  - name: resize_1080p
    image: capsule://wasm-registry/resize.wasm
    trigger: on-read
    args:
      width: "1920"
      height: "1080"
    resources:
      max_memory_pages: 16
      fuel_limit: 10000000
    verification:
      sha256: "<hex>"
      signature: "<optional ed25519 sig>"
```

## Execution Model

- **OnRead (default):** decrypt -> decompress -> transform chain -> client stream
- **OnWrite:** transform chain -> compress/dedup -> encrypt -> persist

## WASM ABI (v0)

WASM modules are loaded as raw binaries. The guest exports:

- `memory` (linear memory)
- `alloc(len: u32) -> u32`
- `dealloc(ptr: u32, len: u32)`
- `process(ptr: u32, len: u32) -> u64` returning `(out_ptr << 32) | out_len`

The host copies the input chunk into guest memory, calls `process`, then copies the
output back out (streaming, bounded by chunk size).

## Notes

- `file://...` and `capsule://<UUID>` module images are supported.
- `capsule://...` images are intended to be stored in SPACE itself (dogfooding).
- `verification.signature` is reserved for trusted publishers (signature verification is planned).
- Derived-output caching is a planned optimization (transform hash + args).
- Enable the implementation by building `pipeline` with `--features phase5`, or via higher-level flags like `spacectl --features phase5` (which enables the modular pipeline path).

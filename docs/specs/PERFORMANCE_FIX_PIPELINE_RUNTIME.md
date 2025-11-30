# Performance Fix: Async Runtime Bridging

**Severity:** Critical  
**Target:** `crates/capsule-registry/src/pipeline.rs`  
**Status:** Proposed

## Problem Analysis
- The synchronous Pipeline API (`write_capsule`, `read_capsule`, etc.) wraps async implementations behind `block_on_future`.
- Current behavior builds `tokio::runtime::Runtime::new()` on every call. Runtime setup (thread pool, I/O drivers, timers) takes milliseconds, dominating operations that should complete in microseconds.
- Under load, per-call runtime creation causes thread churn, FD exhaustion, and scheduler thrashing when many short-lived runtimes are spawned.

## Proposed Solution: Singleton Runtime
- Keep one global Tokio runtime for all synchronous bridge calls using `std::sync::OnceLock` (Rust 1.70+).
- Lazy-init on first use; all subsequent calls share the same pool.
- Configure via `tokio::runtime::Builder::new_multi_thread()`, `enable_all()`, and a small, bounded worker set to avoid starving hosts embedding the library.

### Safety Considerations
- Calling the blocking wrapper from inside an async context will panic in Tokio. We warn when a current handle is detected so callers switch to the async API (`*_async`).
- The global runtime owns its own worker threads, so normal sync callers remain safe and avoid nested executors.

## Verification Plan
1. **Micro-benchmark (Criterion):** Compare “new runtime per call” vs “global runtime” in `benches/runtime_overhead.rs`; expect ≥100x latency improvement (P99) for the global approach.
2. **Integration:** Existing tests must pass unchanged, ensuring the shared runtime does not interfere with pipeline behavior or modular/podms features.

## Success Criteria
- No per-call runtime creation in the sync bridge hot path.
- Benchmark shows a clear win (global runtime dramatically faster).
- No regressions in existing test suites.

# Performance Fix: BatchQueue Memory Safety

**Severity:** Medium  
**Target:** `crates/scaling/src/batch_queue.rs`  
**Status:** Proposed

## Problem Analysis
- BatchQueue currently flushes on time (`flush_interval`) or item count (`max_batch_size`).
- It does **not** constrain memory footprint, so very large items evade the count limit.
- Example: `max_batch_size = 1000`, items are 10 MB each ⇒ ~10 GB can queue before a flush, risking OOM and host instability.

## Proposed Solution: Hybrid Flush Triggers
- Introduce a byte ceiling `max_batch_bytes`.
- Flush whenever **count >= max_batch_size OR bytes >= max_batch_bytes**.
- Track running payload size (`pending_bytes`) as items are buffered to avoid re-counting under locks.
- Constructor `new()` accepts the byte limit (set to a safe default at higher layers); a convenience helper exposes a 4 MiB default.
- Keep existing stats plumbing; `QueueStats::total_bytes` continues to surface batch footprint for tuning.

## Verification Plan
1. Unit test: enqueue a few large items and assert a flush occurs before the count limit or timer.
2. Regression: ensure count-based flush still triggers immediately.
3. Regression: stats reporting still matches (data + ID sizes) after the change.
4. CI: existing suites remain green to prove no behavior regressions elsewhere.

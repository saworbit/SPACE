# SPACE Core Reset

**Date:** 2026-10-19

## Why we got here

SPACE started as an exploration of a clean storage primitive: one capsule (a universal 128-bit ID) that can be viewed through different protocols without copying data, with a write pipeline that combines compression, content-addressed deduplication, and encryption while preserving dedup.

Early sessions added features quickly — multiple protocol views, distributed mesh (PODMS), gossip, Raft, web interfaces, simulations, and many supporting crates. That produced interesting prototypes and some solid core work (especially the pipeline and the property tests that caught real bugs), but it also created ~25 workspace crates, long builds, high cognitive load, and documentation that often described goals rather than what actually worked.

The market and practical experience with software-defined storage (Ceph and others) showed that unbounded SDS rarely ships well. What survives is a focused, opinionated core that someone can actually run and trust on a single node first.

## What was kept

Only the pieces that prove the core thesis and are in good enough shape to build on:

- Capsule storage and registry
- Modular write/read pipeline (compression → BLAKE3 dedup → XTS-AES-256 encryption with deterministic tweaks that preserve dedup)
- S3-compatible protocol view
- spacectl CLI
- Property-based tests for pipeline invariants
- Supporting crates: common, compression, dedup, encryption, storage / nvram-sim

## What was archived

Everything else (PODMS mesh, gossip, other protocol views, foundry, web UI, vendor stubs, heavy experimental CI, etc.) is preserved on the `archive/pre-reset-2026-10` branch. Those ideas can be revisited later if the single-node core gets real use and feedback.

## Next steps

- Make the modular pipeline the default and remove any remaining legacy bridges.
- Automatic background GC and solid reference counting.
- Polish S3 (multipart, range requests, proper errors) and spacectl UX.
- Keep the feature set small until someone outside the original author is actually using it.

The archive branch exists so nothing is lost. This branch is the clean start.

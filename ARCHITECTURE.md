# SPACE Architecture

**Last updated:** 2026-10-19

Post-reset architecture. Previous distributed and multi-protocol diagrams are on the archive branch.

## Core idea

Everything is a capsule (128-bit ID). The write pipeline does segmentation → compression → BLAKE3 content hash → dedup check → XTS-AES-256 encryption (deterministic tweak from the hash so dedup is preserved) → store. The read path reverses it with MAC verification. The same capsule can be viewed through an S3-compatible API or the spacectl CLI without extra copies.

## Current crates

- `common` — shared types (CapsuleId, SegmentId, Policy)
- `compression`, `dedup`, `encryption` — pipeline stages
- `storage` / `nvram-sim` — persistent segment log
- `pipeline` / `capsule-registry` — modular write/read pipeline and metadata registry
- `protocol-s3` — S3 view
- `spacectl` — CLI

## Data flow (write)

Client → segment → compress → hash → dedup (reuse or new) → encrypt with deterministic tweak → append to log → update registry.

## Status

Single-node only. Property tests cover the main pipeline invariants. See [RESET.md](RESET.md) and [README.md](README.md).

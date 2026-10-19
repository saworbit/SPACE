# SPACE

Storage Platform for Adaptive Computational Ecosystems

**One capsule. Multiple views. Zero copies.**

> Pre-alpha research project. Not production-ready. Single-developer. APIs will change.

## Core reset (2026-10-19)

This branch is the hard restart. Early exploration produced a lot of crates and experimental features (distributed mesh, extra protocol views, web UI, etc.). That increased cognitive load and made the project hard to ship. The current state keeps only the pieces that still make sense and have been tested:

- Single-node capsule storage
- Modular write/read pipeline: compression → BLAKE3 content-addressed dedup → XTS-AES-256 encryption (deterministic tweaks preserve dedup)
- S3-compatible protocol view
- spacectl CLI
- Property-based tests for pipeline invariants

Everything else is preserved on the `archive/pre-reset-2026-10` branch. See [RESET.md](RESET.md) for the full explainer.

## What works today

- Capsule create / read / stream
- Compression (LZ4 / Zstd) + deduplication + encryption working together
- Basic S3 PUT / GET / DELETE
- spacectl for local operations
- Property tests covering round-trip, dedup under encryption, and related invariants

## Quick start

```bash
git clone https://github.com/saworbit/SPACE.git
cd SPACE
git checkout core-reset
cargo build --workspace
cargo test --workspace
```

Then use `spacectl` or the S3 gateway (see crate docs).

## Status

| Area | Status |
|------|--------|
| Capsule storage + registry | Beta |
| Write/read pipeline (compress + dedup + encrypt) | Beta |
| S3 view | Alpha |
| spacectl | Alpha |
| Property tests | Present and useful |
| Distributed / other protocols / web UI | Archived |

## License

MIT OR Apache-2.0

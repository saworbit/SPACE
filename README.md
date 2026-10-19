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

`cargo check --workspace` and `cargo test --workspace` pass on this branch.

- Capsule create / read / stream through the registry pipeline
- Compression (LZ4 / Zstd), BLAKE3 content-addressed dedup, and XTS-AES-256 encryption
- Modular pipeline crate (`pipeline`). Capsule-registry and spacectl opt into it with `--features modular_pipeline`
- Basic S3 PUT / GET / DELETE
- `spacectl` for local create, read, list, and `serve-s3`
- Property tests in `crates/capsule-registry/tests/proptest_pipeline.rs` (round-trip, encrypted round-trip, dedup)

The workspace members are `common`, `compression`, `dedup`, `encryption`, `storage`, `nvram-sim`, `pipeline`, `capsule-registry`, `protocol-s3`, and `spacectl`.

## Quick start

```bash
git clone https://github.com/saworbit/SPACE.git
cd SPACE
git checkout core-reset
cargo test --workspace
cargo run -p spacectl -- --help
```

## Status

| Area | Status |
|------|--------|
| Capsule storage + registry | Beta |
| Write/read pipeline (compress + dedup + encrypt) | Beta |
| S3 view | Alpha |
| spacectl | Alpha |
| Property tests | Passing on the default workspace test run |
| Distributed mesh, other protocol views, web UI | Removed from this branch; history is on `archive/pre-reset-2026-10` |

## License

MIT OR Apache-2.0

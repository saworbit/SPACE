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
- Compression (LZ4 / Zstd) and BLAKE3 content-addressed dedup
- XTS-AES-256 encryption with a BLAKE3 MAC. `spacectl create` and S3 PUT turn it on when `SPACE_MASTER_KEY` is set.
- Modular pipeline (`pipeline`): the registry write and read path
- Basic S3: buckets, PUT / GET / HEAD / DELETE, ranged GETs, and ListObjects; see [the S3 quick start](docs/guides/QUICKSTART_S3.md).
- On-disk format 1: segments are framed, checksummed records in `space.nvram`, and capsule writes and deletes, S3 buckets and S3 keys are committed to a journal, `space.journal`. `space.db` is an index of the journal and can be rebuilt from it ([spec](docs/on-disk-format.md)).
- Background maintenance in `serve-s3`: it reclaims space (compacting `space.nvram`, or deleting unreferenced segment files under `SPACE_STORAGE_ROOT`) and scrubs every segment on a schedule.
- `spacectl` for local create, read, list, delete, `gc`, `compact`, `scrub`, and `serve-s3`
- Property tests in `crates/capsule-registry/tests/proptest_pipeline.rs` (round-trip, encrypted round-trip, dedup)

The workspace members are `common`, `compression`, `dedup`, `encryption`, `storage`, `nvram-sim`, `pipeline`, `capsule-registry`, `protocol-s3`, and `spacectl`.

Known gaps are tracked in [GitHub issues](https://github.com/saworbit/SPACE/issues).

## Quick start

Requires Rust 1.89 or newer.

```bash
git clone https://github.com/saworbit/SPACE.git
cd SPACE
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

SPACE is licensed under the [Business Source License 1.1](LICENSE). You can read, change and redistribute the code, and use it in production for noncommercial purposes. Commercial production use needs a commercial license from the licensor. Each version becomes available under the Apache License 2.0 four years after it is first published.

Versions up to commit `a54ed95` (13 November 2026) were released under MIT OR Apache-2.0, and copies of them stay under those terms.

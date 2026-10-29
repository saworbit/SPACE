# SPACE Roadmap

**Last updated:** 2026-10-19

This is the post-reset roadmap. The previous multi-phase plan (including distributed PODMS, extra protocol views, and foundry) is archived on `archive/pre-reset-2026-10`.

See [RESET.md](RESET.md) for why the hard restart happened and what was kept.

## Guiding principles

1. Prove the single-node core thesis first.
2. Keep the surface small.
3. Documentation matches what actually builds and tests.

## Current focus (core capsule)

- Modular pipeline as the default path.
- Automatic background GC and working reference counting.
- S3 view: multipart and range requests.
- spacectl polish (progress, doctor, config, completions).
- Property tests and basic benchmarks for the pipeline.
- External review of the encryption crate when the core is stable.

## Later (only after real use)

Distributed features, additional protocol views, and experimental crates can be reconsidered if the single-node artifact gets external use. Until then they stay on the archive branch.

## Tactical next steps

- Local cargo check / test on this branch.
- Make modular pipeline default and remove any remaining legacy bridges.
- Background GC.
- S3 and CLI polish items above.

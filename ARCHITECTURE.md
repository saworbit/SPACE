# SPACE Architecture

**Last updated:** 2026-11-13

Post-reset architecture. Previous distributed and multi-protocol diagrams are on the archive branch.

## Core idea

Everything is a capsule (128-bit ID). The write pipeline does segmentation → compression → BLAKE3 content hash → dedup check → XTS-AES-256 encryption (deterministic tweak from the hash so dedup is preserved) → store. The read path reverses it, verifying the MAC on encrypted segments and the content hash on unencrypted ones. The same capsule can be read through the S3-compatible API or the spacectl CLI without extra copies, though not at the same time: both open the same sled database ([#92](https://github.com/saworbit/SPACE/issues/92)).

## Current crates

- `common` — shared types (CapsuleId, SegmentId, Policy)
- `compression`, `dedup`, `encryption` — pipeline stages
- `storage` / `nvram-sim` — segment storage: the data log, `space.nvram` (framed records, compacted by writing its next generation), and the file backends
- `pipeline` / `capsule-registry` — modular write/read pipeline; the capsule journal, `space.journal`, and its sled index, `space.db`; the scrub executor
- `protocol-s3` — S3 view
- `spacectl` — CLI

## Data flow (write)

Client → segment → compress → hash → dedup (reuse or new) → encrypt with deterministic tweak → append a `SEGMENT` record to the data log → commit a `CAPSULE` record to the journal (the commit point) → apply it to the sled index.

The data log and the journal are the truth; `space.db` can be rebuilt from them. The byte layouts are in [on-disk-format.md](docs/on-disk-format.md).

## Background maintenance

`serve-s3` runs two loops. `spacectl` runs each on demand, with the server stopped:

- **Reclaim** (`--reclaim-interval`; `spacectl gc` and `spacectl compact`): compacts `space.nvram` into its next generation once enough of it is unreferenced, or deletes unreferenced segment files under `SPACE_STORAGE_ROOT`. Then it compacts `space.journal` once enough of it is dead: deleted and overwritten capsules, and removed S3 keys. See [capsule.md §7](docs/capsule.md#7-deletion-ref-counting-and-gc).
- **Scrub** (`--scrub-interval` and `--deep-scrub-interval`; `spacectl scrub`): light scrub checks each record's checksums, and deep scrub checks MACs and content hashes. See [capsule.md §8](docs/capsule.md#8-scrub-continuous-integrity).

## Status

Single-node only. Property tests cover the main pipeline invariants. See [RESET.md](RESET.md) and [README.md](README.md).

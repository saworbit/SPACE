# SPACE Core Scope

**Last updated:** 2026-10-19

Post-reset scope. The previous v0.2 plan is superseded by this document and [RESET.md](RESET.md).

## Goal

A shippable single-node artifact that proves the core thesis: one capsule, with a write/read pipeline that does compression + BLAKE3 dedup + XTS-AES-256 encryption (dedup preserved), exposed via S3 and spacectl.

## In scope

- Single-node capsule CRUD, stream, range.
- Modular pipeline (compress → hash → dedup → encrypt → store) as the default.
- Automatic background GC and reference counting that reclaims space.
- S3: PUT/GET/HEAD/LIST/DELETE, multipart, range requests, streaming, proper errors, encryption transparent to clients.
- spacectl: create/read/delete/list, serve-s3, doctor, progress, config, completions.
- Property tests for pipeline invariants.
- Basic metrics and health.

## Out of scope (archived)

PODMS mesh, gossip, Raft control plane, federation, foundry, other protocol views (NFS/block/NVMe/FUSE/CSI), web interface, WASM transforms, post-quantum, eBPF gateway, multi-node everything. All preserved on `archive/pre-reset-2026-10`.

## Next concrete steps

1. Confirm the reduced workspace builds and tests pass.
2. Modular pipeline default; remove legacy bridge.
3. Background GC.
4. S3 multipart + range + error mapping.
5. spacectl doctor and progress.

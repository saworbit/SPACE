# Protocol Views Integration Guide

SPACE treats every piece of data as a capsule and projects that capsule
graph into multiple protocol facades. This guide captures the current state
of those facades and explains how to exercise them with the `spacectl` CLI.

> **At a glance**
>
> - Capsules remain the sole durability primitive
> - Each protocol view is a lightweight in-process adapter
> - Metadata for stateful views is persisted alongside `space.nvram`

---

## 1. Current protocol adapters

| Protocol | Crate | Backing state | Purpose |
|----------|-------|---------------|---------|
| S3 (object) | `protocol-s3` | in-memory key map | REST proof-of-concept for object workloads |
| NFS-style namespace | `protocol-nfs` | `space.nfs.json` | Directory + file hierarchy backed by capsules |
| Block volume facade | `protocol-block` | `space.block.json` | Logical volumes with copy-on-write rewrites |

All adapters share the same `WritePipeline` implementation from
`capsule-registry`.  That pipeline handles compression, dedupe,
encryption, reference counting, and segment GC, so protocol-specific code
can focus on simple metadata concerns.

---

## 2. NFS namespace view

The NFS facade provides a POSIX-like directory tree. Paths are always
normalised to `/`-prefixed POSIX form, regardless of the host OS.

- **Crate:** `crates/protocol-nfs`
- **Persistence:** `space.nfs.json` (created next to `space.nvram`)
- **Key operations:** `mkdir`, `write_file`, `read_file`, `delete`, `list_directory`

Every mutating operation writes the namespace map back to disk so the
directory structure survives process restarts.  When a file is overwritten,
the old capsule is deleted via the pipeline to keep reference counts honest.

**CLI usage**

```powershell
# Create hierarchy and write a file from disk
spacectl nfs mkdir --path /analytics/raw
spacectl nfs write --path /analytics/raw/data.json --file sample.json

# Inspect and read back
spacectl nfs list --path /analytics/raw
spacectl nfs metadata --path /analytics/raw/data.json
spacectl nfs read --path /analytics/raw/data.json > roundtrip.json

# Remove a file or empty directory
spacectl nfs delete --path /analytics/raw/data.json
```

---

## 3. Block protocol view

The block façade presents logical LUNs that are internally stored as capsules.
Writes currently rewrite the full capsule to keep consistency simple and to
leverage dedupe/encryption in the pipeline.

- **Crate:** `crates/protocol-block`
- **Persistence:** `space.block.json`
- **Key operations:** `create_volume`, `list_volumes`, `read`, `write`, `delete_volume`

Each volume tracks size, block size, the capsule ID for the latest data,
and a monotonically increasing version used to reject concurrent writers.

**CLI usage**

```powershell
# Create a new 16 MiB logical volume
spacectl block create vol0 16777216

# Write data from a local file at offset 4096
spacectl block write vol0 4096 --file sector.bin

# Read back 512 bytes to stdout
spacectl block read vol0 4096 --length 512 > verify.bin

# Inspect and remove
spacectl block info vol0
spacectl block delete vol0
```

---

## 4. Capsule inventory support

`spacectl list` now walks the capsule registry directly, reporting size and
segment counts for every known capsule.  This helps correlate protocol-level
operations with the underlying capsule activity.

---

## 5. Test coverage

Two dedicated persistence tests demonstrate the restart behaviour for the new
views:

- `crates/protocol-nfs/tests/nfs_view_test.rs::nfs_persists_namespace_state`
- `crates/protocol-block/tests/block_view_test.rs::block_persists_volumes_across_reopen`

These tests create data, drop the view, reopen it, and confirm both metadata
and payload integrity.

---

## 6. Operational notes

- The protocol JSON files are intentionally human-readable.  Treat them as
  diagnostic artefacts during development; production deployments would move
  to a durable metadata store.
- Deleting a file or volume removes its mapping and schedules the underlying
  capsule for GC via the pipeline.  Segments with shared dedupe references are
  retained until every referencing capsule is removed.
- Future work will layer proper servers (NFSv4, NVMe-oF) atop these façades.
  The current in-process adapters establish the API contracts required for
  those services.

---

*Last updated: 2025-10-16*

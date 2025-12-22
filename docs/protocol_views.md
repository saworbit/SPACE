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

### Phase 4 projections (feature-gated)

Phase 4 adds “Views”: lightweight, stateless protocol adapters that translate legacy I/O into pipeline reads.

| Protocol | Crate | Entry point | Notes |
|----------|-------|-------------|------|
| Local projection (“content” view) | `protocol-fuse` | `mount_capsule_fuse` (Unix + `kernel_fuse`), `mount_fuse_view` (fallback) | Read-only kernel FUSE mount on Unix (`/content`) when built with kernel support; otherwise streams into a `content` file |
| CSI (Kubernetes) | `protocol-csi` | `publish_capsule_volume` | Stub helper; publishes via the local projection view |
| NFS export | `protocol-nfs::phase4` | `export_nfs_view` | Registers an export via the vendored `nfs-rs` server |
| NVMe-oF | `protocol-nvme` | `NvmeView::project` | SPDK-backed projection scaffolding (simulated) |

**CLI usage (Phase 4)**

```bash
# Build with Phase 4 enabled
cargo build -p spacectl --features phase4

# Store a file as a capsule (optionally in a zone)
./target/debug/spacectl put ./hello.txt --id <uuid> --zone zone-a

# Project it locally as a legacy-compatible file view
./target/debug/spacectl project mount --id <uuid> --target /tmp/space-view --zone zone-a
cat /tmp/space-view/content
```

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
- Tiering (Phase 6) is implemented at the storage backend layer; protocol views continue to use the same read APIs and will transparently rehydrate cold segments when enabled. See `docs/guides/TIERING.md`.
- Deleting a file or volume removes its mapping and schedules the underlying
  capsule for GC via the pipeline.  Segments with shared dedupe references are
  retained until every referencing capsule is removed.
- Phase 4 introduces early protocol projection scaffolding (NVMe/NFS/CSI + a local mount view). Kernel-backed FUSE is now available on Unix (experimental); other kernel integrations remain roadmap items.

---

*Last updated: 2025-12-20*

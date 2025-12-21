# Phase 4: Advanced Protocol Views & Full Mesh Federation

> ## ⚠️ PLANNED FEATURES - MOSTLY NOT IMPLEMENTED
>
> **Status:** Phase 4 is partially implemented behind the `phase4` feature flag (simulation-first). An experimental read-only kernel FUSE mount is available on Unix when built with `spacectl` feature `kernel_fuse` and system `libfuse3` headers installed.
>
> **Reality Check:**
> - Protocol projections exist (helpers): `protocol-nvme`, `protocol-nfs::phase4`, `protocol-csi`
> - Local projection mount exists: `spacectl project mount` (prefers a read-only kernel FUSE mount on Unix; falls back to a `content`-file view)
> - Federation (Phase 4b) uses a gRPC WAN bridge: `Policy.federation.targets` + `spacectl zone add` + `spacectl federation serve`
> - Phase 5 (planned/early): `Policy.transform` attaches WASM transforms to read/write streaming (see `docs/phase5.md`)
>
> This document mixes current behavior and aspirational architecture. When unsure, treat it as a guide for the `phase4` feature implementation.
>
> **For actual feature status, see the main [README Feature Status Table](../README.md#-feature--capability-status).**

## Purpose & Goals

*Phase 4 aims to realize the* "one capsule, infinite views" *vision by projecting capsules as NVMe-oF, NFS v4.2, FUSE, and CSI surfaces without materializing extra copies, while sharding metadata with Paxos for sovereign, low-latency federation.*

Goals:

1. Project capsules into multiple view pipelines with zero-copy re-encryption/recompression hooks.
2. Extend PODMS scaling with Raft-powered metadata shards, zone-aware routing, and telemetry-driven federation.
3. Gate new functionality behind `phase4` so single-node users have no regressions.
4. Provide CLI, docs, and scripts that prove the mesh works end-to-end (NVMe discovery, CSI provisioning, geo federation).

**Current Reality:** Phase 4 is still experimental, but it is no longer “docs-only”: you can create a capsule, mount a view, and (optionally) replicate into another zone without changing client tooling.

## Scope & Assumptions

- Linux hosts with SPDK-friendly toolchains, eBPF, and optionally RDMA hardware (Mellanox/ConnectX or mocks).
- Docker/Kind for system tests; no Windows/macOS support yet.
- Zonal policy compiler (PODMS Step 3) already wired through `common::podms` and the Scaling Agent.
- All new code lives in `crates/protocol-nvme`, `crates/protocol-nfs`, `crates/protocol-fuse`, `crates/protocol-csi`, and `crates/scaling` under the `phase4` feature.

## Architecture & Actions

### Views

- `protocol-nvme` returns `NvmeView` backed by `spdk-rs` namespaces. It calls `policy_compiler::compile_scaling` (via `scaling::compiler`) with `Telemetry::ViewProjection` to emit `ScalingAction::Federate`/`ShardEC` hooks.
- `protocol-nfs` exposes `export_nfs_view()` returning a running `nfs-rs::NfsServer`. Federation actions mirror the NVMe flow.
- `protocol-fuse` provides the local projection mount: a read-only kernel FUSE filesystem on Unix (exposing `/content`), with a portable `content`-file view fallback elsewhere.
- `protocol-csi` provisions Kubernetes volumes through `csi-driver-rs` (stub) and publishes capsules via the local view mount.

Each protocol forwards actions to `MeshNode::federate_capsule` and `MeshNode::shard_metadata`, which now talks to a lightweight `raft-rs` cluster stub storing shards per zone.

### Federation

- `MeshNode` uses `RaftCluster::{new, for_zone}` and `ShardKey::new` when sharding metadata, writing serialized capsule records to Raft logs (stubbed in `vendor/raft-rs`).
- Capsules derive deterministic shard IDs via `CapsuleId::shard_keys(count)`.
- The CLI triggers these flows through the `spacectl project --view <nvme|nfs|fuse|csi>` command (see below).
- For *payload* replication across zones, use `crates/federation::Bridge` with `spacectl zone add` + `spacectl federation serve` (see `scripts/test_federation_mock.sh`).


### Security & Transformation

- Protocol views read via the shared pipeline (`WritePipeline::read_capsule` / `read_range`), which performs decompression/decryption based on stored segment metadata and capsule policy.
- Policy enforcement is centralized: all Phase 4 views invoke `scaling::enforce_view_policy` before projection so federation/sharding actions can execute prior to exposing the view.

### CLI Command


```bash
cargo run -p spacectl -- project \
  --view nvme \
  --id 550e8400-e29b-41d4-a716-446655440000 \
  --policy-file examples/phase4-policy.yaml
```

- The command loads a YAML policy, spins up a minimal `MeshNode` (Metro zone, `127.0.0.1:0`), and routes to the right protocol helper.
- Policies can request sovereignty/latency targets and optional federation rules via `federation` (targets + priority).
- Enable the entire pipeline with `cargo build --features phase4` or `spacectl --features phase4 project ...`.

**Local projection mount (recommended for dev validation)**

```bash
# 1) Store a local file as a capsule (optionally with federation targets)
spacectl put ./hello.txt --id 550e8400-e29b-41d4-a716-446655440000 --policy-file examples/phase4-policy.yaml

# 2) Project it into a directory containing `content`
spacectl project mount --id 550e8400-e29b-41d4-a716-446655440000 --target /tmp/space-view

# 3) Use standard tooling
cat /tmp/space-view/content
```

## Tests & Benchmarks

1. **Unit Tests per crate**
   - `protocol-nvme` ensures `project_nvme_view` returns an `NvmeView` and exercises Raft stubs.
   - `protocol-nfs` reuses the metadata assertions and validates the `NfsServer` is configured even after federation.
   - `protocol-fuse` and `protocol-csi` have tokio tests for the portable view mount/provisioning handles (kernel FUSE is Unix-only).
2. **Integration idea**
   - Multi-node KIND scenario (Phase4 script) writes capsules, projects an NFS view, federates to a geo zone, and re-reads data.
3. **Security / Chaos**
   - `scripts/test_federation_resilience.sh` is currently a local Phase 3 Raft failover smoke test (3 nodes, leader kill, metadata ops continue). A Chaos Mesh/KIND partition harness is a future add-on.
4. **Benchmarks (future)**
   - Use Criterion for `project_nvme_view` latency (<50ms) and `MeshNode::federate_capsule` (<100?s) by mocking RDMA loops.
5. **Smoke scripts**
    - `scripts/test_phase4.sh` runs `spacectl project --view nvme` and validates NVMe discovery output end-to-end.
    - `scripts/test_phase4_projection.sh` runs `spacectl put` + `spacectl project mount` and verifies `cat <mount>/content` works.

## Scripts & Deployments

- `scripts/test_phase4_views.sh`: Builds `spacectl` with `--features phase4`, runs a KIND multi-node cluster (`deployment/kind-config.yaml`), projects NVMe/NFS/CSI views, and relies on `kind` `kubectl` to deploy the driver (`deployment/csi-driver.yaml`).
- `scripts/test_federation_resilience.sh`: Local 3-node Phase 3 Raft metadata failover smoke test.

### Deployment Assets

- `deployment/kind-config.yaml` describes a 3-node cluster (control-plane + 2 workers) with port mappings for NVMe/TCP.
- `deployment/csi-driver.yaml` is a namespaced Deployment + Service for the CSI driver built from `spacectl`.

## Timeline (5-6 week push)

1. **Week 1**: Bootstrap `phase4` feature, add protocol crates, confirm `phase4` gating.
2. **Week 2**: Wire new views, metadata sharding, `MeshNode` federation, and CLI hooks.
3. **Week 3**: Integration scripts + KIND/CAPI manifest; add tests.
4. **Week 4**: Benchmarks, security validators (eBPF policy gate placeholders, Chaos testing).
5. **Week 5**: Docs, demos, multi-node recordings. (Weeks 5-6 buffer for polish.)

## Risks & Mitigation

- **SPDK, NFS, FUSE dependencies**: We vendor minimal crates (`spdk-rs`, `nfs-rs`, `fuse-rs`, `csi-driver-rs`) as placeholders and keep the hardware-specific logic wrapped in feature gates.
- **Raft/Paxos complexity**: Start with single `MeshNode` shards and a `raft-rs` stub. Replace stub with a negotiable cluster when production hardware is ready.
- **Latency**: Sampling with Criterion and tracing (`tokio::time::Instant`) ensures views stay under 50ms; fall back to TCP/TLS transport when RDMA not present.
- **Kubernetes integration**: Scripts deploy the CSI driver to KIND for sanity checking; the driver is still a facade around `spacectl project csi`.

## FAQ

- **Why now?** Phase 3 proved the universal capsule and PODMS scaling. This phase completes the fabric by adding cross-protocol views and federated metadata.
- **Hardware required?** Linux only today. RDMA/Mellanox optional ? the scripts and vendor crates mock transport with TCP ports.
- **Does single-node mode break?** No. `phase4` is opt-in. Without `--features phase4`, the new crates and CLI path remain unused.
- **Can we add SMB or iSCSI later?** Yes. The new phases expose `project_nvme_view` hooks where future protocols can plug right in.
- **How do we prove compliance?** Logs include tracing spans (`nvme_project`, `nfs_export`, `fuse_mount`, `csi_provision`). `MeshNode` emits `info!` events when shards are stored, making audit chains easy to follow.

Refer to [federation.md](./federation.md) for zonal routing + Raft shard details and [README](README.md) for quick-start commands.

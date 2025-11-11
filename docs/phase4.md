# Phase 4: Advanced Protocol Views & Full Mesh Federation

## Purpose & Goals

*Phase 4 realizes the patentable* “one capsule, infinite views” *thesis by projecting capsules as NVMe-oF, NFS v4.2, FUSE, and CSI surfaces without materializing extra copies, while sharding metadata with Paxos for sovereign, low-latency federation.*

Goals:

1. Project capsules into multiple view pipelines with zero-copy re-encryption/recompression hooks.
2. Extend PODMS scaling with Raft-powered metadata shards, zone-aware routing, and telemetry-driven federation.
3. Gate new functionality behind `phase4` so single-node users have no regressions.
4. Provide CLI, docs, and scripts that prove the mesh works end-to-end (NVMe discovery, CSI provisioning, geo federation).

## Scope & Assumptions

- Linux hosts with SPDK-friendly toolchains, eBPF, and optionally RDMA hardware (Mellanox/ConnectX or mocks).
- Docker/Kind for system tests; no Windows/macOS support yet.
- Zonal policy compiler (PODMS Step 3) already wired through `common::podms` and the Scaling Agent.
- All new code lives in `crates/protocol-nvme`, `crates/protocol-nfs`, `crates/protocol-fuse`, `crates/protocol-csi`, and `crates/scaling` under the `phase4` feature.

## Architecture & Actions

### Views

- `protocol-nvme` returns `NvmeView` backed by `spdk-rs` namespaces. It calls `policy_compiler::compile_scaling` (via `scaling::compiler`) with `Telemetry::ViewProjection` to emit `ScalingAction::Federate`/`ShardEC` hooks.
- `protocol-nfs` exposes `export_nfs_view()` returning a running `nfs-rs::NfsServer`. Federation actions mirror the NVMe flow.
- `protocol-fuse` mounts a FUSE filesystem locally via `fuse-rs`, reusing the same scaling hooks for federation and metadata sharding.
- `protocol-csi` provisions Kubernetes volumes through `csi-driver-rs`, sharding metadata before calling the CSI server.

Each protocol forwards actions to `MeshNode::federate_capsule` and `MeshNode::shard_metadata`, which now talks to a lightweight `raft-rs` cluster stub storing shards per zone.

### Federation

- `MeshNode` now uses `RaftCluster::{new, for_zone}` and `ShardKey::new` when encrypting metadata shards, writing serialized capsule records to Raft logs.
- Capsules derive deterministic shard IDs via `CapsuleId::shard_keys(count)`.
- The CLI triggers these flows through the `spacectl project --view <nvme|nfs|fuse|csi>` command (see below).

### CLI Command

```bash
cargo run -p spacectl -- project \
  --view nvme \
  --id 550e8400-e29b-41d4-a716-446655440000 \
  --policy-file examples/phase4-policy.yaml
```

- The command loads a YAML policy, spins up a minimal `MeshNode` (Metro zone, `127.0.0.1:0`), and routes to the right protocol helper.
- Policies live in `examples/phase4-policy.yaml` and can request zero RPO, 2ms latency, and zone sovereignty by editing `rpo`, `latency_target`, and `sovereignty` fields.
- Enable the entire pipeline with `cargo build --features phase4` or `spacectl --features phase4 project ...`.

## Tests & Benchmarks

1. **Unit Tests per crate**
   - `protocol-nvme` ensures `project_nvme_view` returns an `NvmeView` and exercises Raft stubs.
   - `protocol-nfs` reuses the metadata assertions and validates the `NfsServer` is configured even after federation.
   - `protocol-fuse` and `protocol-csi` have tokio tests that mount/provision volumes and assert the handles are live.
2. **Integration idea**
   - Multi-node KIND scenario (Phase4 script) writes capsules, projects an NFS view, federates to a geo zone, and re-reads data.
3. **Security / Chaos**
   - `scripts/test_federation_resilience.sh` injects partitions (Chaos Mesh) to ensure Raft shards maintain consistency.
4. **Benchmarks (future)**
   - Use Criterion for `project_nvme_view` latency (<50ms) and `MeshNode::federate_capsule` (<100µs) by mocking RDMA loops.

## Scripts & Deployments

- `scripts/test_phase4_views.sh`: Builds `spacectl` with `--features phase4`, runs a KIND multi-node cluster (`deployment/kind-config.yaml`), projects NVMe/NFS/CSI views, and relies on `kind` `kubectl` to deploy the driver (`deployment/csi-driver.yaml`).
- `scripts/test_federation_resilience.sh`: Installs Chaos Mesh into KIND, partitions pods, and ensures `spacectl project` still returns consistent metadata.

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
- **Hardware required?** Linux only today. RDMA/Mellanox optional — the scripts and vendor crates mock transport with TCP ports.
- **Does single-node mode break?** No. `phase4` is opt-in. Without `--features phase4`, the new crates and CLI path remain unused.
- **Can we add SMB or iSCSI later?** Yes. The new phases expose `project_nvme_view` hooks where future protocols can plug right in.
- **How do we prove compliance?** Logs include tracing spans (`nvme_project`, `nfs_export`, `fuse_mount`, `csi_provision`). `MeshNode` emits `info!` events when shards are stored, making audit chains easy to follow.

Refer to [docs/federation.md](docs/federation.md) for zonal routing + Raft shard details and [README](README.md) for quick-start commands.

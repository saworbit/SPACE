# Phase 4: Advanced Protocol Views & Full Mesh Federation

Phase 4 realizes the “one capsule, infinite views” promise by projecting the universal capsule namespace onto NVMe-oF, NFS/FUSE, and CSI surfaces while federating metadata across distributed mesh nodes. All new Rust code is gated behind the `phase4` feature so operators opt into the expanded surface consciously.

## Crate Map

- `protocol-nvme`: Exposes `project_nvme_view` to turn a capsule into an NVMe namespace. It consults `scaling::MeshNode` to resolve federated targets, migrate the capsule when sovereignty/latency demands it, and shard metadata entries through `shard_metadata`.
- `protocol-nfs::phase4`: Builds a policy-aware export that triggers federation when `Policy::latency_target` is ultra-low and logs the export path. It reuses the existing NFS namespace code while keeping the new view code isolated under feature flags.
- `protocol-fuse`: Offers `mount_fuse_view`, delivering a local FUSE mount that respects policy guards and serves from the same capsule metadata as the other views.
- `protocol-csi`: Provides `csi_provision_volume`, which federates capsules, shards metadata, and hands back a CSI-friendly volume identifier for Kubernetes.
- `scaling::MeshNode`: Gains `resolve_federated`, `federate_capsule`, and `shard_metadata` helpers plus the `MetadataShard` descriptor so distributed nodes can answer “where is capsule X?” in ~100µs while respecting policy sovereignty constraints.
- `capsule-registry::pipeline`: The async write path emits telemetry, and when `phase4` is enabled it uses the mesh node to federate capsules whose `Policy::sovereignty` is not `Local`.

## Federation Flow

```
CapsuleRegistry  -> write pipeline -> MeshNode (phase4 helpers)
         ↳ protocol-nvme -> NVMe-oF target
         ↳ protocol-nfs::phase4 + protocol-fuse -> file mounts
         ↳ protocol-csi -> Kubernetes CSI volume
```

- Mesh nodes gossip peers, select targets, and call `federate_capsule` before exposing a view.
- Metadata shards (`MetadataShard`) keep the capsule-to-node mapping distributed, so federated reads consult a handful of owners instead of every mesh peer.
- Policies that request low latency or global sovereignty trigger `mesh.federate_capsule` before the view becomes available.

## Policy Template

See `examples/phase4-policy.yaml` for a declarative capsule policy that binds NVMe projection, metro federation, and audit-level QoS controls. A snippet:

```yaml
capsule:
  name: "prod-data"
  policy:
    latency_target: 2ms
    sovereignty: zone
    view: nvme
    federate: metro
```

## Testing & Scripts

- `test_phase4.sh` spins up a lightweight KIND cluster, builds with `cargo build --features phase4`, deploys the CSI driver, and exercises NVMe/NFS projections end-to-end.
- `test_federation_failover.sh` simulates mesh node churn to prove federated views stay consistent when targets fail.
- Benchmarks should spot-check that projection latency stays under 100µs and federation emits <50ms migrations.

## Activation

1. Build with `cargo build --features phase4`.
2. Use `spacectl --view nvme|nfs|fuse|csi --policy-file examples/phase4-policy.yaml`.
3. Deploy `deployment/csi-deployment.yaml` when using the CSI driver in Kubernetes.
4. Run the scripts above to validate federation, failover, and policy compliance before enabling the feature in production.
